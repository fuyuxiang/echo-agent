//! Permission resolution engine: merges native `.echo-agent/config.toml`,
//! managed/enterprise settings, and (via `claude_settings`) `.claude`
//! settings into the effective `PermissionConfig`; MCP/marketplace
//! allowlists; always-approve policy.

use crate::permission::claude_settings::*;
use crate::permission::rules::*;

use std::path::{Path, PathBuf};
use std::str::FromStr;

use tracing::{debug, info, warn};

use crate::permission::types::{
    PatternMode, PermissionConfig, PermissionRule, PromptPolicy, RuleAction, ToolFilter,
};

/// Whether user/project/local files should apply their own `defaultMode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UserDefaultModeLoad {
    /// Apply most-specific user/project/local `defaultMode`.
    Apply,
    /// Managed-settings already owns the mode — load allow/deny/ask only.
    SkipManagedOwns,
}

/// Synthetic rules + skip records for `acceptEdits` / `bypassPermissions`.
///
/// Shared by managed and user-tier application so pin handling cannot drift.
fn synthetic_rules_for_default_mode(
    mode: DefaultPermissionMode,
    policy_block: Option<&str>,
) -> (
    Vec<PermissionRule>,
    Vec<SkippedPermission>,
    bool, /* bypass_blocked */
) {
    let effects = mode.effects();
    let mut rules = Vec::new();
    let mut skipped = Vec::new();
    let mut bypass_blocked = false;

    if effects.bypass_permissions {
        if let Some(reason) = policy_block {
            warn!("defaultMode=bypassPermissions ignored: disabled by managed policy");
            bypass_blocked = true;
            skipped.push(SkippedPermission {
                rule: "defaultMode=bypassPermissions".to_string(),
                reason: reason.to_string(),
            });
        } else {
            debug!("defaultMode=bypassPermissions: appending catch-all Allow Any rule");
            rules.push(PermissionRule {
                action: RuleAction::Allow,
                tool: ToolFilter::Any,
                pattern: None,
                pattern_mode: PatternMode::Glob,
            });
        }
    } else if effects.accept_edits {
        debug!("defaultMode=acceptEdits: appending synthetic Allow Edit rule");
        rules.push(PermissionRule {
            action: RuleAction::Allow,
            tool: ToolFilter::Edit,
            pattern: None,
            pattern_mode: PatternMode::Glob,
        });
    }

    (rules, skipped, bypass_blocked)
}

/// Parse a raw defaultMode string: unknown → [`DefaultPermissionMode::Default`]
/// (fail-safe) with a warn + skip record for `echoagent inspect`.
fn parse_default_mode_claiming_scope(
    raw: &str,
    path: &Path,
    skipped: &mut Vec<SkippedPermission>,
) -> DefaultPermissionMode {
    match DefaultPermissionMode::from_str(raw) {
        Ok(mode) => mode,
        Err(invalid) => {
            warn!(
                path = %path.display(),
                default_mode = %invalid,
                "settings: unrecognized defaultMode value; treating as default (prompt)"
            );
            skipped.push(SkippedPermission {
                rule: format!("defaultMode={invalid}"),
                reason: "unrecognized value; treated as default".to_string(),
            });
            DefaultPermissionMode::Default
        }
    }
}

/// Parse `[permission]` from TOML. Tries compact (`deny = ["Read(...)"]`) first,
/// falls back to verbose (`[[permission.rules]]`).
fn parse_toml_permission_section(
    permission_value: &toml::Value,
) -> Result<Vec<PermissionRule>, String> {
    let mut rules = Vec::new();
    let mut found_compact = false;

    for (action, key) in [
        (RuleAction::Deny, "deny"),
        (RuleAction::Allow, "allow"),
        (RuleAction::Ask, "ask"),
    ] {
        if let Some(value) = permission_value.get(key) {
            let Some(arr) = value.as_array() else {
                // Don't drop a security rule list silently.
                warn!(
                    "permission.{key}: expected an array of rule strings, got {} -- ignored",
                    toml_type_name(value)
                );
                continue;
            };
            found_compact = true;
            for (i, item) in arr.iter().enumerate() {
                if let Some(s) = item.as_str() {
                    match parse_permission_rule(s, action) {
                        Ok(rule) => rules.push(rule),
                        Err(e) => warn!("permission.{key}[{i}]: \"{s}\" -- {e}"),
                    }
                } else {
                    warn!(
                        "permission.{key}[{i}]: expected string, got {}",
                        toml_type_name(item)
                    );
                }
            }
        }
    }

    if found_compact {
        return Ok(rules);
    }

    permission_value
        .clone()
        .try_into::<PermissionConfig>()
        .map(|config| config.rules)
        .map_err(|e| e.to_string())
}

fn toml_type_name(v: &toml::Value) -> &'static str {
    match v {
        toml::Value::String(_) => "string",
        toml::Value::Integer(_) => "integer",
        toml::Value::Float(_) => "float",
        toml::Value::Boolean(_) => "boolean",
        toml::Value::Datetime(_) => "datetime",
        toml::Value::Array(_) => "array",
        toml::Value::Table(_) => "table",
    }
}

use crate::permission::types::{RequirementSource, Sourced};

/// Try to extract `[permission]` rules from a TOML config value.
fn extract_toml_permissions(
    config: &toml::Value,
    make_source: impl Fn() -> RequirementSource,
) -> Vec<Sourced<PermissionRule>> {
    let Some(permission_value) = config.get("permission") else {
        return Vec::new();
    };
    match parse_toml_permission_section(permission_value) {
        Ok(rules) => {
            let source = make_source();
            if !rules.is_empty() {
                info!(count = rules.len(), %source, "Loaded permission rules");
            }
            rules
                .into_iter()
                .map(|rule| Sourced {
                    value: rule,
                    source: source.clone(),
                })
                .collect()
        }
        Err(e) => {
            let source = make_source();
            warn!(error = %e, %source, "Failed to parse [permission]");
            Vec::new()
        }
    }
}

/// Load `[permission]` rules from requirements.toml layers. Trust keys on the
/// `is_system` flag (set at load, never from `path`): system → `SystemRequirements`,
/// user `~/.echo-agent` → `Requirements`, so [`is_admin_source`] trusts only the root tier.
fn load_requirements_permissions() -> Vec<Sourced<PermissionRule>> {
    echo_agent_config::requirements_layers()
        .into_iter()
        .flat_map(|layer| {
            let source = if layer.is_system {
                RequirementSource::SystemRequirements {
                    path: PathBuf::from(layer.source.label().as_ref()),
                }
            } else {
                RequirementSource::Requirements {
                    path: PathBuf::from(layer.source.label().as_ref()),
                }
            };
            extract_toml_permissions(&layer.value, || source.clone())
        })
        .collect()
}

/// Load `[permission]` rules from native EchoAgent TOML config files:
///
///   * `~/.echo-agent/config.toml` (lowest priority)
///   * Each `.echo-agent/config.toml` from the git repo root down to `cwd`
///     (highest priority last) — same walk as folder-trust's
///     [`crate::project_config::find_project_configs`] so detector and loader
///     cannot disagree on which project configs exist.
///
/// Returns the rules tagged with `RequirementSource::Config`. Empty if no
/// config file contains a `[permission]` section.
fn load_config_toml_permissions(cwd: &Path, project_trusted: bool) -> Vec<Sourced<PermissionRule>> {
    let mut rules = Vec::new();

    // Global `~/.echo-agent/config.toml` first (lowest priority within this layer).
    // Gated on user_echo_agent_home() so a project's .echo-agent/config.toml is never read as
    // global permissions when neither ECHO_AGENT_HOME nor a home dir resolves.
    if let Some(global_path) =
        echo_agent_config::user_echo_agent_home().map(|g| g.join("config.toml"))
        && global_path.is_file()
    {
        match echo_agent_config::load_config_file(&global_path) {
            Ok(value) => rules.extend(extract_toml_permissions(&value, || {
                RequirementSource::Config {
                    path: global_path.clone(),
                }
            })),
            Err(e) => {
                warn!(path = %global_path.display(), error = %e, "Failed to load global config.toml")
            }
        }
    }

    // Project-scoped configs walking from git root down to cwd, gated on trust.
    // An untrusted clone must not contribute allow/deny/ask rules via
    // `.echo-agent/config.toml` (same gate as project `.claude/settings.json`).
    if project_trusted {
        for path in crate::project_config::find_project_configs(cwd) {
            match echo_agent_config::load_config_file(&path) {
                Ok(value) => rules.extend(extract_toml_permissions(&value, || {
                    RequirementSource::Config { path: path.clone() }
                })),
                Err(e) => {
                    warn!(path = %path.display(), error = %e, "Failed to load project config.toml")
                }
            }
        }
    }

    rules
}

fn managed_config_permissions(
    layers: &[echo_agent_config::ManagedConfigLayer],
) -> Vec<Sourced<PermissionRule>> {
    layers
        .iter()
        .flat_map(|layer| {
            extract_toml_permissions(&layer.value, || RequirementSource::ManagedConfig {
                path: layer.path.clone(),
            })
        })
        .collect()
}

// ═════════════════════════════════════════════════════════════════════════════
// Fallback Resolver
// ═════════════════════════════════════════════════════════════════════════════

/// Resolve permission config, merging native EchoAgent and Claude sources.
/// Evaluation is order-independent (deny > ask > allow); merge order affects
/// provenance display only.
///
/// `defaultMode: "acceptEdits"` in Claude settings generates a synthetic
/// `Allow Edit` rule appended to the Claude rules.
///
/// `project_trusted` gates project-tier `.claude/settings.json` and
/// `.echo-agent/config.toml` permission rules (mirrors [`load_claude_env_with_project`]).
/// Global/user/admin tiers always load. Callers pass the folder-trust bridge
/// verdict for local sessions; hub/cloud defaults trusted.
pub async fn resolve_permission_config_with_fallback(
    cwd: &Path,
    project_trusted: bool,
) -> Option<PermissionConfig> {
    resolve_permissions_with_provenance(cwd, project_trusted)
        .await
        .map(|r| r.config)
}

/// Patterns of `Deny` rules that forbid *reading* a path — those on `Read`,
/// `Grep`, or `Any` (the tools that surface file contents). Write-only denies
/// (`Edit`/`Write`/`Bash`) and non-deny actions are excluded.
///
/// Public so a caller holding the manager's *effective* config (managed +
/// claude fallback + CLI `--deny`) can derive the Grep tool's ripgrep excludes
/// from that same config, rather than re-resolving managed-only and missing CLI
/// read denies.
pub fn deny_read_globs_from_config(config: &PermissionConfig) -> Vec<String> {
    config
        .rules
        .iter()
        .filter(|r| {
            r.action == RuleAction::Deny
                && matches!(
                    r.tool,
                    ToolFilter::Read | ToolFilter::Grep | ToolFilter::Any
                )
        })
        .filter_map(|r| r.pattern.clone())
        .collect()
}

/// Result of permission resolution with provenance metadata.
pub struct ResolvedPermissions {
    pub config: PermissionConfig,
    /// `sources[i]` is where `config.rules[i]` came from.
    pub sources: Vec<RequirementSource>,
    /// Rules from `.claude/settings.json` that couldn't be parsed (empty for TOML).
    pub skipped: Vec<SkippedPermission>,
}

/// A permission rule that was recognized but not loaded.
pub struct SkippedPermission {
    pub rule: String,
    pub reason: String,
}

fn tag_with_source(
    target: &mut Vec<Sourced<PermissionRule>>,
    rules: Vec<PermissionRule>,
    source: RequirementSource,
) {
    target.extend(rules.into_iter().map(|rule| Sourced {
        value: rule,
        source: source.clone(),
    }));
}

/// Whether an Allow rule is a blanket `--yolo` substitute the pin must drop: a
/// catch-all on `Any` or a dangerous freeform dimension (Bash/MCP/WebFetch),
/// detected via [`rule_is_catchall`]. Read/Edit/Grep are file-access only, so a
/// catch-all on them is not a substitute and survives.
pub fn is_catchall_allow(rule: &PermissionRule) -> bool {
    if rule.action != RuleAction::Allow {
        return false;
    }
    // File-access tools (no command execution) are never `--yolo` substitutes.
    if matches!(
        rule.tool,
        ToolFilter::Read | ToolFilter::Edit | ToolFilter::Grep
    ) {
        return false;
    }
    crate::permission::policy::rule_is_catchall(rule)
}

/// Root-owned tiers whose catch-all allows survive the pin (managed-settings,
/// system requirements). Keyed on provenance, never a spoofable `path`.
fn is_admin_source(source: &RequirementSource) -> bool {
    matches!(
        source,
        RequirementSource::SystemRequirements { .. } | RequirementSource::ManagedSettings { .. }
    )
}

/// Under the pin, drop untrusted catch-all Allow rules (they substitute for the
/// blocked `--yolo`); keep admin-tier ones. Records each drop for `echoagent inspect`.
fn drop_untrusted_catchall_allows(
    rules: Vec<Sourced<PermissionRule>>,
    policy_block: Option<&'static str>,
    skipped: &mut Vec<SkippedPermission>,
) -> Vec<Sourced<PermissionRule>> {
    let Some(reason) = policy_block else {
        return rules;
    };
    rules
        .into_iter()
        .filter(|sourced| {
            if is_catchall_allow(&sourced.value) && !is_admin_source(&sourced.source) {
                warn!(
                    source = %sourced.source,
                    "catch-all allow rule ignored: always-approve disabled by managed policy"
                );
                skipped.push(SkippedPermission {
                    rule: format!(
                        "allow {} (catch-all)",
                        sourced.value.pattern.as_deref().unwrap_or("*")
                    ),
                    reason: reason.to_string(),
                });
                false
            } else {
                true
            }
        })
        .collect()
}

/// Inputs to [`resolve_permissions_with_provenance_inner`]. Production uses
/// [`ResolveInputs::live`]; tests construct the fields directly so no test
/// reads the host's real managed files through this seam.
struct ResolveInputs<'a> {
    policy_block: Option<&'static str>,
    managed: &'a ManagedSettings,
    managed_config_rules: Vec<Sourced<PermissionRule>>,
    /// Folder-trust verdict for `cwd`. When false, project-tier
    /// `.claude/settings.json` / `.echo-agent/config.toml` permission rules are dropped
    /// (global/user/admin tiers still load).
    project_trusted: bool,
}

impl ResolveInputs<'static> {
    fn live(project_trusted: bool) -> Self {
        Self {
            policy_block: yolo_disabled_by_policy(),
            managed: managed_settings(),
            managed_config_rules: managed_config_permissions(
                &echo_agent_config::managed_config_layers(),
            ),
            project_trusted,
        }
    }
}

/// Collect permission rules from every source, keeping each rule's origin:
/// requirements.toml, managed-settings.json, managed_config.toml,
/// config.toml, and .claude/settings.json. A deny always wins over an ask,
/// and an ask over an allow, no matter which file a rule comes from; the
/// source order above only affects how origins are displayed.
///
/// Rules are read when a session starts. Changes take effect in the next
/// session.
///
/// `permissions.defaultMode` from **managed-settings** outranks user/project/local
/// for the *mode* scalar (managed scope wins). User-tier defaultMode is
/// applied only when managed does not set one.
///
/// **Always-approve (yolo) is independent of defaultMode:** session always-approve
/// still auto-approves before [`PromptPolicy::Deny`] (`dontAsk`) is consulted,
/// so always-approve outranks `defaultMode` unless
/// bypass is pinned off via echoagent `requirements.toml`
/// (`[ui] disable_bypass_permissions_mode = true`). Pair managed `dontAsk` with
/// that pin when org policy must not be bypassable by `--always-approve`.
///
/// `project_trusted` gates project-tier Claude settings and `.echo-agent/config.toml`
/// permission rules the same way [`load_claude_env_with_project`] gates env.
/// Without this, an untrusted clone can ship `defaultMode: bypassPermissions`
/// or broad allow rules and disable approval prompts.
pub async fn resolve_permissions_with_provenance(
    cwd: &Path,
    project_trusted: bool,
) -> Option<ResolvedPermissions> {
    resolve_permissions_with_provenance_inner(cwd, ResolveInputs::live(project_trusted)).await
}

async fn resolve_permissions_with_provenance_inner(
    cwd: &Path,
    inputs: ResolveInputs<'_>,
) -> Option<ResolvedPermissions> {
    let ResolveInputs {
        policy_block,
        managed,
        managed_config_rules,
        project_trusted,
    } = inputs;
    let config_toml_rules = load_config_toml_permissions(cwd, project_trusted);

    // Managed defaultMode wins; skip user-tier defaultMode application so a
    // project acceptEdits cannot loosen a managed dontAsk/auto/default.
    let managed_mode = managed.default_mode;
    let user_mode_load = if managed_mode.is_some() {
        UserDefaultModeLoad::SkipManagedOwns
    } else {
        UserDefaultModeLoad::Apply
    };

    // Phase 2 cutoff: skip the .claude/ fallback once the user has imported.
    // Native config-derived permissions still apply.
    let skip_claude = is_claude_import_marked_with_log("resolve_permissions_with_provenance");
    let settings_json = if skip_claude {
        None
    } else {
        resolve_claude_settings_inner(cwd, project_trusted, policy_block, user_mode_load)
    };

    let mut all_rules: Vec<Sourced<PermissionRule>> = Vec::new();
    all_rules.extend(load_requirements_permissions());
    all_rules.extend(managed.permissions.clone());

    let mut skipped = Vec::new();
    let mut prompt_policy = PromptPolicy::default();

    // Apply managed defaultMode synthetics + prompt policy (highest mode tier).
    if let Some(mode) = managed_mode {
        prompt_policy = mode.effects().prompt_policy;
        let managed_path = managed
            .features
            .source_path
            .clone()
            .unwrap_or_else(|| PathBuf::from("managed-settings.json"));
        let source = RequirementSource::ManagedSettings { path: managed_path };
        let (syn_rules, syn_skipped, _) = synthetic_rules_for_default_mode(mode, policy_block);
        skipped.extend(syn_skipped);
        for rule in syn_rules {
            all_rules.push(Sourced {
                value: rule,
                source: source.clone(),
            });
        }
    }

    all_rules.extend(managed_config_rules);
    all_rules.extend(config_toml_rules);
    if let Some((config, skipped_rules, path)) = settings_json {
        skipped.extend(skipped_rules);
        // User-tier prompt_policy only when managed did not set defaultMode.
        if managed_mode.is_none() {
            prompt_policy = config.prompt_policy;
        }
        tag_with_source(
            &mut all_rules,
            config.rules,
            RequirementSource::Settings { path },
        );
    }

    // Must run while provenance is in scope (discarded by the unzip below). CLI
    // `--allow '*'` is filtered at its own merge site (acp_session).
    let all_rules = drop_untrusted_catchall_allows(all_rules, policy_block, &mut skipped);

    // Keep skip-only resolutions alive so the drop reaches `echoagent inspect`; zero
    // rules with Ask is a no-op for the evaluator, identical to the `None` arm.
    if all_rules.is_empty() && prompt_policy == PromptPolicy::Ask && skipped.is_empty() {
        return None;
    }

    let (rules, sources): (Vec<_>, Vec<_>) =
        all_rules.into_iter().map(|s| (s.value, s.source)).unzip();

    debug!(rules = rules.len(), "Resolved permission rules");

    Some(ResolvedPermissions {
        config: PermissionConfig {
            rules,
            prompt_policy,
        },
        sources,
        skipped,
    })
}

/// Resolve permissions from Claude settings, merging allow/deny/ask across all
/// settings scopes so broad global grants aren't dropped when a project file also
/// exists. `defaultMode` is not merged: the most-specific file that sets it wins
/// (including unrecognized values, which claim the slot as `default` — an
/// unknown → default fail-safe).
///
/// `defaultMode` handling:
///   - `bypassPermissions`: catch-all `Allow Any`, but ignored (recorded as a
///     [`SkippedPermission`]) when [`yolo_disabled_by_policy`] pins bypass off
///   - `acceptEdits`: synthetic `Allow Edit`
///   - `default` / `plan`: no synthetic rules
///   - `dontAsk`: [`PromptPolicy::Deny`] (unapproved tools auto-denied)
///   - `auto`: [`PromptPolicy::Auto`] (classifier; seeded on the manager)
///
/// When [`UserDefaultModeLoad::SkipManagedOwns`], only allow/deny/ask rules are
/// loaded from user/project/local files.
///
/// Synthetic rules are appended last as fallbacks (explicit deny still wins).
/// `policy_block` is threaded for testability; prod passes the live pin.
/// When `project_trusted` is false, only global `~/.claude` settings load —
/// project-tree rules and `defaultMode` are dropped (same gate as env injection).
fn resolve_claude_settings_inner(
    cwd: &Path,
    project_trusted: bool,
    policy_block: Option<&'static str>,
    user_mode_load: UserDefaultModeLoad,
) -> Option<(PermissionConfig, Vec<SkippedPermission>, PathBuf)> {
    let mut all_rules = Vec::new();
    let mut all_skipped = Vec::new();
    let mut primary_source_path: Option<PathBuf> = None;
    // Track defaultMode from the most specific file (paths are most-specific-first).
    // Also track its source path so synthetic rules have provenance even when
    // no explicit permissions block exists.
    let mut default_mode_source: Option<PathBuf> = None;
    let mut applied_mode: Option<DefaultPermissionMode> = None;
    let mut prompt_policy = PromptPolicy::default();
    let mut files_with_rules: u32 = 0;

    // Same path set as env injection ([`claude_settings_paths_for_trust`]).
    for path in claude_settings_paths_for_trust(cwd, project_trusted) {
        let Some(settings) = load_claude_settings(&path) else {
            continue;
        };

        if let Some(dirs) = &settings.additional_directories {
            info!(
                path = %path.display(),
                count = dirs.len(),
                "Claude settings: additionalDirectories parsed but not supported"
            );
        }

        // defaultMode: most-specific file that *sets* the key wins — including
        // typos (treated as default). Skipped when managed-settings owns mode.
        if user_mode_load == UserDefaultModeLoad::Apply
            && default_mode_source.is_none()
            && let Some(raw) = &settings.default_mode
        {
            default_mode_source = Some(path.clone());
            let mode = parse_default_mode_claiming_scope(raw, &path, &mut all_skipped);
            applied_mode = Some(mode);
            prompt_policy = mode.effects().prompt_policy;
        }

        if let Some(perms) = settings.permissions {
            let (cfg, warnings) = perms.into_permission_config();
            for w in &warnings {
                warn!(path = %path.display(), "{}", w);
            }
            // Rules *or* skip-only parse failures still own provenance for
            // `echoagent inspect` (all-invalid allow/deny/ask must not leave
            // primary_source_path unset and panic below).
            if (!cfg.rules.is_empty() || !warnings.is_empty()) && primary_source_path.is_none() {
                primary_source_path = Some(path.clone());
            }
            if !cfg.rules.is_empty() {
                files_with_rules += 1;
                debug!(
                    path = %path.display(),
                    rules = cfg.rules.len(),
                    "Claude settings: loaded permission rules"
                );
            }
            all_rules.extend(cfg.rules);
            all_skipped.extend(warnings.into_iter().map(|w| {
                let (rule, reason) = w
                    .split_once(" \u{2014} ")
                    .or_else(|| w.split_once(" -- "))
                    .map_or((w.as_str(), ""), |(r, d)| (r, d));
                SkippedPermission {
                    rule: rule.to_string(),
                    reason: reason.to_string(),
                }
            }));
        }
    }

    let mut bypass_blocked = false;
    if let Some(mode) = applied_mode {
        let (syn_rules, syn_skipped, blocked) =
            synthetic_rules_for_default_mode(mode, policy_block);
        bypass_blocked = blocked;
        all_skipped.extend(syn_skipped);
        all_rules.extend(syn_rules);
    }

    // A blocked bypass, a claimed defaultMode (incl. typo→default), or skip
    // records still resolve (possibly zero rules) so provenance reaches
    // `echoagent inspect` via the outer resolver.
    if all_rules.is_empty()
        && prompt_policy == PromptPolicy::Ask
        && !bypass_blocked
        && default_mode_source.is_none()
        && all_skipped.is_empty()
    {
        return None;
    }

    if files_with_rules > 1 {
        info!(
            files = files_with_rules,
            total_rules = all_rules.len(),
            "Claude settings: merged permission rules from multiple files"
        );
    }

    // Prefer the first file with explicit permission rules or skip-only
    // parse failures; fall back to the file that provided defaultMode.
    // Never panic: a skip-only / mode-only resolution must always surface.
    let source_path = primary_source_path
        .or(default_mode_source)
        .unwrap_or_else(|| {
            warn!(
                cwd = %cwd.display(),
                skipped = all_skipped.len(),
                "Claude settings resolution has no settings file provenance; using cwd"
            );
            cwd.to_path_buf()
        });

    Some((
        PermissionConfig {
            rules: all_rules,
            prompt_policy,
        },
        all_skipped,
        source_path,
    ))
}

// ═════════════════════════════════════════════════════════════════════════════
// managed-settings.json
// ═════════════════════════════════════════════════════════════════════════════

use std::sync::OnceLock;

/// Claude `managed-settings.json` subset we load.
///
/// **Supported surface today:** a single file from
/// [`echo_agent_config::claude_managed_settings_path`] (platform path such as
/// `/Library/Application Support/ClaudeCode/managed-settings.json`). We do
/// **not** yet merge Claude's `managed-settings.d/` drop-ins, MDM plist, or
/// Windows registry delivery.
#[derive(Debug, Default)]
pub struct ManagedSettings {
    pub features: ManagedSettingsFeatures,
    pub permissions: Vec<Sourced<PermissionRule>>,
    /// Parsed `permissions.defaultMode` (highest mode precedence over user files).
    default_mode: Option<DefaultPermissionMode>,
    pub mcp_allowlist: McpServerAllowlist,
    pub marketplace_allowlist: MarketplaceAllowlist,
}

static MANAGED_SETTINGS: OnceLock<ManagedSettings> = OnceLock::new();

pub fn managed_settings() -> &'static ManagedSettings {
    MANAGED_SETTINGS.get_or_init(load_managed_settings)
}

fn load_managed_settings() -> ManagedSettings {
    let Some(path) = echo_agent_config::claude_managed_settings_path() else {
        return ManagedSettings::default();
    };
    let Some(json) = read_managed_settings_json(&path) else {
        return ManagedSettings::default();
    };
    parse_managed_settings_json(&json, &path)
}

fn parse_managed_settings_json(json: &serde_json::Value, path: &Path) -> ManagedSettings {
    let env = json.get("env");
    let features = ManagedSettingsFeatures {
        disable_telemetry: json_env_flag(env, "DISABLE_TELEMETRY"),
        disable_feedback: json_env_flag(env, "DISABLE_FEEDBACK_COMMAND"),
        disable_yolo: parse_disable_bypass_permissions(json),
        source_path: Some(path.to_path_buf()),
    };

    let mcp_allow_entries = parse_mcp_entries(json, ALLOWED_MCP_SERVERS_KEY);
    let mcp_deny_entries = parse_mcp_entries(json, DENIED_MCP_SERVERS_KEY);

    if !mcp_allow_entries.is_empty() {
        info!(
            path = %path.display(),
            count = mcp_allow_entries.len(),
            "Loaded MCP server allowlist"
        );
    }
    if !mcp_deny_entries.is_empty() {
        info!(
            path = %path.display(),
            count = mcp_deny_entries.len(),
            "Loaded MCP server denylist"
        );
    }

    let marketplace_urls: Vec<String> = json
        .get("strictKnownMarketplaces")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|entry| {
                    let source = entry.get("source")?.as_str()?;
                    if source != "git" {
                        return None;
                    }
                    entry.get("url").and_then(|u| u.as_str()).map(String::from)
                })
                .collect()
        })
        .unwrap_or_default();

    if !marketplace_urls.is_empty() {
        info!(
            path = %path.display(),
            count = marketplace_urls.len(),
            "Loaded marketplace allowlist"
        );
    }

    let permissions = parse_managed_settings_permissions(json, path);
    let mut skipped = Vec::new();
    let default_mode = extract_default_mode(json, path).map(|raw| {
        let mode = parse_default_mode_claiming_scope(&raw, path, &mut skipped);
        info!(
            path = %path.display(),
            default_mode = %raw,
            "Loaded permissions.defaultMode from managed-settings.json"
        );
        for s in &skipped {
            warn!(path = %path.display(), rule = %s.rule, reason = %s.reason, "managed defaultMode");
        }
        mode
    });

    ManagedSettings {
        features,
        permissions,
        default_mode,
        mcp_allowlist: McpServerAllowlist::new(
            mcp_allow_entries,
            mcp_deny_entries,
            Some(path.to_path_buf()),
        ),
        marketplace_allowlist: MarketplaceAllowlist {
            allowed_urls: marketplace_urls,
            source_path: Some(path.to_path_buf()),
        },
    }
}

const ALLOWED_MCP_SERVERS_KEY: &str = "allowedMcpServers";
const DENIED_MCP_SERVERS_KEY: &str = "deniedMcpServers";

/// Parse `serverUrl` → Http, `command` → Stdio, `serverName` → Name (the keys
/// Claude's MCP policy supports). A dropped deny entry = silent zero
/// enforcement, so unsupported `deniedMcpServers` keys `warn!`; the allow side
/// stays silent (an ungranted entry is fail-closed).
fn parse_mcp_entries(json: &serde_json::Value, key: &str) -> Vec<AllowedMcpServer> {
    let Some(arr) = json.get(key).and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    let mut entries = Vec::new();
    for entry in arr {
        if let Some(url) = entry.get("serverUrl").and_then(|u| u.as_str()) {
            if key == ALLOWED_MCP_SERVERS_KEY {
                warn_on_unmatchable_allow_url(url);
            } else {
                warn_on_unmatchable_deny_url(url);
            }
            entries.push(AllowedMcpServer::Http {
                url_pattern: url.to_string(),
            });
        } else if let Some(cmd) = entry.get("command").and_then(|c| c.as_str()) {
            entries.push(AllowedMcpServer::Stdio {
                command: cmd.to_string(),
            });
        } else if let Some(name) = entry.get("serverName").and_then(|n| n.as_str()) {
            entries.push(AllowedMcpServer::Name {
                name: name.to_string(),
            });
        } else if key == DENIED_MCP_SERVERS_KEY {
            warn!(
                entry = %entry,
                "ignoring unsupported deniedMcpServers entry; only serverUrl, command, and serverName are honored"
            );
        }
    }
    entries
}

/// Allow matching compares schemes literally ([`url_allow_matches`]), so a
/// pattern with no scheme or a glob in the scheme (`*://host/*`,
/// `*.corp.com/*`) can never match a runtime URL. That fails closed, but a
/// fleet policy written this way silently loses its grants — tell the admin.
fn warn_on_unmatchable_allow_url(pattern: &str) {
    match split_scheme(pattern) {
        (None, _) => warn!(
            pattern,
            "allowedMcpServers serverUrl has no scheme; schemes match literally, so this entry can never match — write e.g. https://{pattern}"
        ),
        (Some(scheme), _) if scheme.contains(['*', '?', '[']) => {
            warn!(
                pattern,
                "allowedMcpServers serverUrl has a glob in its scheme; schemes match literally, so this entry can never match — write the scheme out (e.g. https://)"
            );
        }
        (Some(_), rest) => {
            let (authority, _) = split_authority_path(rest);
            let authority = authority.rsplit('@').next().unwrap_or(authority);
            if is_bracketed_ipv6(authority) {
                // Bracketed IPv6 compares by parsed address — any spelling
                // of the same address matches. Its port still matches as a
                // literal number, so a glob or non-numeric port grants
                // nothing, same as on a domain host.
                let suffix = authority.split_once(']').map_or("", |(_, rest)| rest);
                let port_ok = suffix.is_empty()
                    || suffix
                        .strip_prefix(':')
                        .is_some_and(|p| p.parse::<u16>().is_ok());
                if !port_ok {
                    warn!(
                        pattern,
                        "allowedMcpServers serverUrl port is not a number; ports match literally, so this entry can never match"
                    );
                }
                return;
            }
            // Warn against the MATCHER's view of the pattern (host = before
            // the last `:`, trailing dots trimmed like canonicalization) —
            // a shape the matcher already handles must not warn: a false
            // "can never match" trains admins to ignore load-bearing warns.
            let (host, port) = match authority.rsplit_once(':') {
                Some((h, p)) => (h, Some(p)),
                None => (authority, None),
            };
            let host = host.trim_end_matches('.');
            // Ports are literal numbers; a glob or non-numeric port grants
            // nothing — the pre-split whole-URL glob semantics honored
            // `:*`, so legacy configs must hear about the change.
            if !host.contains(':')
                && let Some(p) = port
                && p.parse::<u16>().is_err()
            {
                warn!(
                    pattern,
                    "allowedMcpServers serverUrl port is not a number; ports match literally, so this entry can never match"
                );
                return;
            }
            if host.is_empty() {
                warn!(
                    pattern,
                    "allowedMcpServers serverUrl has no host; this entry can never match"
                );
            } else if let Ok(v6) = host.parse::<std::net::Ipv6Addr>() {
                // The matcher's host text works iff it equals the canonical
                // spelling the runtime side reports (`…::1:443` with a
                // canonical address + port works; `2001:0db8::1:443` not).
                if v6.to_string() != host {
                    warn!(
                        pattern,
                        canonical = %v6,
                        "allowedMcpServers serverUrl IPv6 host is not in canonical form and can never match — write the canonical spelling or bracket the address"
                    );
                }
            } else if host.contains(':') && !host.contains(['*', '?', '[']) {
                // A colon-bearing LITERAL that isn't a valid address (the
                // port split ate part of an unbracketed IPv6); a glob host
                // like `2001:db8*` can still match and stays silent.
                warn!(
                    pattern,
                    "allowedMcpServers serverUrl IPv6 host must be bracketed; as written this entry can never match"
                );
            } else if let Some(ip) = parse_ip_host(host)
                && ip.to_string() != host
            {
                // Allow hosts glob-match the canonical runtime host, so an
                // IP in a non-canonical spelling (`127.1`, `0xa9fea9fe`)
                // can never match its connect target.
                warn!(
                    pattern,
                    canonical = %ip,
                    "allowedMcpServers serverUrl IP host is not in canonical form and can never match — write the canonical spelling"
                );
            } else if glob::Pattern::new(&canonicalize_pattern_host(host)).is_err() {
                warn!(
                    pattern,
                    "allowedMcpServers serverUrl host glob does not compile; this entry can never match"
                );
            } else {
                warn_on_dead_unicode_glob_label(pattern, host, "allowedMcpServers");
            }
        }
    }
}

/// A deny entry that can never match is silent zero enforcement — tell the
/// admin. Covers a host-less pattern, a host glob that doesn't compile
/// (matching nothing, since no parseable runtime host contains `[`), and a
/// label mixing Unicode with glob chars.
fn warn_on_unmatchable_deny_url(pattern: &str) {
    let (host, _) = split_host_path(pattern);
    match host {
        None => warn!(
            pattern,
            "deniedMcpServers serverUrl has no host; this entry can never match"
        ),
        Some(host) => {
            if glob::Pattern::new(&canonicalize_pattern_host(&host)).is_err() {
                warn!(
                    pattern,
                    "deniedMcpServers serverUrl host glob does not compile; the entry matches nothing"
                );
            }
            warn_on_dead_unicode_glob_label(pattern, &host, "deniedMcpServers");
        }
    }
}

/// A host label mixing non-ASCII with glob metacharacters can never match:
/// runtime hosts are punycoded, and a partial label can't be.
fn warn_on_dead_unicode_glob_label(pattern: &str, host: &str, key: &str) {
    if host
        .split('.')
        .any(|label| !label.is_ascii() && label.contains(['*', '?', '[']))
    {
        warn!(
            pattern,
            "{key} serverUrl mixes Unicode and glob characters in one host label; runtime hosts are punycoded, so this entry can never match"
        );
    }
}

fn parse_managed_settings_permissions(
    json: &serde_json::Value,
    path: &Path,
) -> Vec<Sourced<PermissionRule>> {
    let Some(perms_value) = json.get("permissions") else {
        return Vec::new();
    };
    let permissions: ParsedPermissions = match serde_json::from_value(perms_value.clone()) {
        Ok(p) => p,
        Err(_) => return Vec::new(),
    };
    let (config, warnings) = permissions.into_permission_config();
    for w in &warnings {
        warn!(path = %path.display(), "{}", w);
    }
    if !config.rules.is_empty() {
        info!(
            path = %path.display(),
            count = config.rules.len(),
            "Loaded permission rules from managed-settings.json"
        );
    }
    let source = RequirementSource::ManagedSettings {
        path: path.to_path_buf(),
    };
    config
        .rules
        .into_iter()
        .map(|rule| Sourced {
            value: rule,
            source: source.clone(),
        })
        .collect()
}

fn read_managed_settings_json(path: &Path) -> Option<serde_json::Value> {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => {
            warn!(path = %path.display(), error = %e, "Failed to read managed-settings.json");
            return None;
        }
    };
    match serde_json::from_str(&content) {
        Ok(v) => Some(v),
        Err(e) => {
            warn!(path = %path.display(), error = %e, "Failed to parse managed-settings.json");
            None
        }
    }
}

#[derive(Debug, Default)]
pub struct ManagedSettingsFeatures {
    pub disable_telemetry: Option<bool>,
    pub disable_feedback: Option<bool>,
    pub disable_yolo: Option<bool>,
    pub source_path: Option<std::path::PathBuf>,
}

pub fn json_env_flag(env: Option<&serde_json::Value>, key: &str) -> Option<bool> {
    let val = env?.get(key)?;
    match val {
        serde_json::Value::Number(n) => Some(n.as_i64().unwrap_or(0) != 0),
        serde_json::Value::Bool(b) => Some(*b),
        serde_json::Value::String(s) => match s.as_str() {
            "0" | "" | "false" => Some(false),
            _ => Some(true),
        },
        _ => None,
    }
}

fn parse_disable_bypass_permissions(json: &serde_json::Value) -> Option<bool> {
    let val = json
        .get("permissions")?
        .get("disableBypassPermissionsMode")?;
    Some(val.as_str() == Some("disable"))
}

/// Shared pin-reason literals ([`yolo_disabled_by_policy`]); the named source
/// tells an admin which file activated the lock.
pub const YOLO_PIN_REASON_REQUIREMENTS: &str = "always-approve disabled by managed policy ([ui] disable_bypass_permissions_mode = true in requirements.toml)";
/// Back-compat: the legacy `[ui] yolo = false` requirements key still locks.
pub const YOLO_PIN_REASON_LEGACY_YOLO: &str =
    "always-approve disabled by managed policy ([ui] yolo = false in requirements.toml)";

/// Hard-lock predicate (client gates, permission manager, vendor bypass gate):
/// `Some(reason)` iff a requirements layer sets `[ui]
/// disable_bypass_permissions_mode = true` (or legacy `[ui] yolo = false`).
/// Vendor `managed-settings.json` `disableBypassPermissionsMode` is deliberately
/// not consulted: echoagent must not inherit a host-wide always-approve lockdown from
/// that file. echoagent still honors that file's permission rules / MCP / marketplace
/// allowlists, and the user's own `--yolo` / `[ui] permission_mode` / runtime
/// toggle drive always-approve; to disable it in echoagent use a root-owned
/// `requirements.toml`. Fails open on user-writable layers.
pub fn yolo_disabled_by_policy() -> Option<&'static str> {
    let layers = echo_agent_config::requirements_layers();
    // The source label only names the layer in the non-bool warning; materialize
    // it as a PathBuf so the borrowed iterator below outlives the temporaries.
    let labeled: Vec<(PathBuf, &toml::Value)> = layers
        .iter()
        .map(|l| (PathBuf::from(l.source.label().as_ref()), &l.value))
        .collect();
    resolve_yolo_policy_block(labeled.iter().map(|(p, v)| (p.as_path(), *v)))
}

/// Read `[ui] <key>` as a bool; a non-bool value warns (naming key + layer)
/// rather than silently failing to lock.
fn requirements_lock_bool(ui: Option<&toml::Value>, key: &str, path: &Path) -> Option<bool> {
    let value = ui?.get(key)?;
    match value.as_bool() {
        Some(b) => Some(b),
        None => {
            warn!(
                path = %path.display(),
                key,
                "[ui] {key} must be a boolean; ignoring non-bool value \
                 (always-approve lock not applied from this key in this layer)"
            );
            None
        }
    }
}

/// Pure form of [`yolo_disabled_by_policy`] over pre-loaded layers (testable
/// without `~/.echo-agent`); `path` only names the layer in a non-bool warning.
fn resolve_yolo_policy_block<'a>(
    requirement_layers: impl Iterator<Item = (&'a Path, &'a toml::Value)>,
) -> Option<&'static str> {
    for (path, layer) in requirement_layers {
        let ui = layer.get("ui");
        // Native lock key (default false). `true` pins always-approve off.
        if requirements_lock_bool(ui, "disable_bypass_permissions_mode", path) == Some(true) {
            return Some(YOLO_PIN_REASON_REQUIREMENTS);
        }
        // Back-compat alias: `[ui] yolo = false` in requirements.toml still pins
        // (pre-rename configs). A config.toml `yolo` is unaffected (not read here).
        if requirements_lock_bool(ui, "yolo", path) == Some(false) {
            return Some(YOLO_PIN_REASON_LEGACY_YOLO);
        }
    }
    None
}

#[derive(Debug, Clone)]
pub enum AllowedMcpServer {
    Http {
        url_pattern: String,
    },
    Stdio {
        command: String,
    },
    /// Match by config name (any transport); see [`mcp_name_matches`].
    Name {
        name: String,
    },
}

/// MCP server policy from managed-settings.json: `allowedMcpServers` plus
/// `deniedMcpServers`. Deny takes precedence over allow (deny-wins semantics).
#[derive(Debug, Clone, Default)]
pub struct McpServerAllowlist {
    pub entries: Vec<AllowedMcpServer>,
    pub deny_entries: Vec<AllowedMcpServer>,
    url_patterns: Vec<String>,
    commands: Vec<String>,
    names: Vec<String>,
    deny_url_patterns: Vec<String>,
    deny_commands: Vec<String>,
    deny_names: Vec<String>,
    pub source_path: Option<std::path::PathBuf>,
}

fn split_mcp_entries(entries: &[AllowedMcpServer]) -> (Vec<String>, Vec<String>, Vec<String>) {
    let mut url_patterns = Vec::new();
    let mut commands = Vec::new();
    let mut names = Vec::new();
    for entry in entries {
        match entry {
            AllowedMcpServer::Http { url_pattern } => url_patterns.push(url_pattern.clone()),
            AllowedMcpServer::Stdio { command } => commands.push(command.clone()),
            AllowedMcpServer::Name { name } => names.push(name.clone()),
        }
    }
    (url_patterns, commands, names)
}

impl McpServerAllowlist {
    /// Build a policy from raw allow/deny entries. Public so the enforcement
    /// chokepoint can be exercised in tests without a managed-settings.json on
    /// disk (the runtime path goes through [`parse_managed_settings_json`]).
    pub fn new(
        entries: Vec<AllowedMcpServer>,
        deny_entries: Vec<AllowedMcpServer>,
        source_path: Option<std::path::PathBuf>,
    ) -> Self {
        let (url_patterns, commands, names) = split_mcp_entries(&entries);
        let (deny_url_patterns, deny_commands, deny_names) = split_mcp_entries(&deny_entries);
        Self {
            entries,
            deny_entries,
            url_patterns,
            commands,
            names,
            deny_url_patterns,
            deny_commands,
            deny_names,
            source_path,
        }
    }

    pub fn is_restricted(&self) -> bool {
        !self.entries.is_empty() || !self.deny_entries.is_empty()
    }

    /// URL-only (no name-deny check); use `is_server_allowed` for policy. Test-only.
    #[cfg(test)]
    fn is_http_allowed(&self, url: &str) -> bool {
        if self
            .deny_url_patterns
            .iter()
            .any(|pat| url_deny_matches(pat, url))
        {
            return false;
        }
        if self.url_patterns.is_empty() {
            return true;
        }
        self.url_patterns
            .iter()
            .any(|pat| url_allow_matches(pat, url))
    }

    /// Command-only (no name-deny check); use `is_server_allowed` for policy. Test-only.
    #[cfg(test)]
    fn is_stdio_allowed(&self, command: &str) -> bool {
        if self.deny_commands.iter().any(|c| c == command) {
            return false;
        }
        if self.commands.is_empty() {
            return true;
        }
        self.commands.iter().any(|c| c == command)
    }

    /// Check whether an MCP server is allowed by this policy.
    ///
    /// Deny beats allow. `serverName` is a transport-agnostic dimension enforced
    /// here at the server level; allow is a union across dimensions (match any
    /// applicable URL/command/name), and a deny-only policy allows the rest.
    pub fn is_server_allowed(&self, server: &agent_client_protocol::McpServer) -> bool {
        if !self.is_restricted() {
            return true;
        }
        if self.is_server_denied(server) {
            return false;
        }

        // `restricted` stays false for a deny-only policy, allowing the rest.
        let mut restricted = false;
        let mut matched = false;

        // Name and URL/command allows are a union — a serverName allow grants any
        // URL (more permissive than a strict URL-precedence scheme).
        if !self.names.is_empty() {
            restricted = true;
            matched |= self
                .names
                .iter()
                .any(|pat| mcp_name_matches(pat, mcp_server_name(server)));
        }

        // Emptiness checks live INSIDE the arms: a match guard that fails
        // would fall through to `_`, and the `_` arm's fail-closed gate must
        // catch only genuinely unknown transports — a command-only allowlist
        // restricts stdio, never HTTP (per-dimension union).
        match server {
            agent_client_protocol::McpServer::Http(agent_client_protocol::McpServerHttp {
                url,
                ..
            })
            | agent_client_protocol::McpServer::Sse(agent_client_protocol::McpServerSse {
                url,
                ..
            }) => {
                if !self.url_patterns.is_empty() {
                    restricted = true;
                    matched |= self
                        .url_patterns
                        .iter()
                        .any(|pat| url_allow_matches(pat, url));
                }
            }
            agent_client_protocol::McpServer::Stdio(agent_client_protocol::McpServerStdio {
                command,
                ..
            }) => {
                if !self.commands.is_empty() {
                    restricted = true;
                    let command = command.to_string_lossy();
                    matched |= self.commands.iter().any(|c| *c == command);
                }
            }
            // `McpServer` is #[non_exhaustive] as of acp 0.10. A transport
            // we can't inspect must not slip a URL/command lockdown (fail
            // closed); name allows were already checked above.
            _ => {
                restricted =
                    restricted || !self.url_patterns.is_empty() || !self.commands.is_empty();
            }
        }

        !restricted || matched
    }

    /// True when the server matches a `deniedMcpServers` entry (vs merely
    /// missing from the allowlist) — lets callers report the right reason.
    /// Includes a transport-agnostic `serverName` deny match.
    pub fn is_server_denied(&self, server: &agent_client_protocol::McpServer) -> bool {
        if self
            .deny_names
            .iter()
            .any(|pat| mcp_name_matches(pat, mcp_server_name(server)))
        {
            return true;
        }
        match server {
            agent_client_protocol::McpServer::Http(agent_client_protocol::McpServerHttp {
                url,
                ..
            })
            | agent_client_protocol::McpServer::Sse(agent_client_protocol::McpServerSse {
                url,
                ..
            }) => self
                .deny_url_patterns
                .iter()
                .any(|pat| url_deny_matches(pat, url)),
            agent_client_protocol::McpServer::Stdio(agent_client_protocol::McpServerStdio {
                command,
                ..
            }) => {
                let command = command.to_string_lossy();
                self.deny_commands.iter().any(|c| *c == command)
            }
            // TODO(acp-0.10): `McpServer` is #[non_exhaustive].
            _ => false,
        }
    }
}

/// Namespace prefix for legacy injected MCP server names (`echo_agent_com_*`).
/// Policy matching still uses this spelling.
pub const MANAGED_MCP_PREFIX: &str = "echo_agent_com_";

/// Max `char` length of a managed runtime name (`echo_agent_com_` + normalized display
/// name), sized to the 64-char tool-name budget. Shared with `mcp_name_matches`
/// so a long policy `serverName` still matches its truncated runtime name.
pub const MANAGED_MCP_NAME_MAX_CHARS: usize = 39;

/// Normalize a bare MCP display name to its runtime spelling (lowercase, spaces
/// → `_`). Shared with `mcp_name_matches` so the policy and runtime sides never
/// drift.
pub fn normalize_managed_name(bare: &str) -> String {
    bare.to_lowercase().replace(' ', "_")
}

/// The transport-agnostic config name of an MCP server.
fn mcp_server_name(server: &agent_client_protocol::McpServer) -> &str {
    match server {
        agent_client_protocol::McpServer::Http(http) => &http.name,
        agent_client_protocol::McpServer::Sse(sse) => &sse.name,
        agent_client_protocol::McpServer::Stdio(stdio) => &stdio.name,
        // TODO(acp-0.10): `McpServer` is #[non_exhaustive]; an unknown
        // transport has no name to match, so it never matches a policy entry.
        _ => "",
    }
}

/// Match a policy `serverName` against a runtime server name: both sides
/// reduce to one key (strip `echo_agent_com_`, [`normalize_managed_name`]) and
/// compare by exact equality — never substring; an empty key never matches.
fn mcp_name_matches(pattern: &str, name: &str) -> bool {
    fn key(s: &str) -> String {
        normalize_managed_name(s.strip_prefix(MANAGED_MCP_PREFIX).unwrap_or(s))
    }
    // Mirror the legacy injected-name truncation: `echo_agent_com_*` runtime
    // names were capped at MANAGED_MCP_NAME_MAX_CHARS total (prefix-inclusive
    // on the bare part).
    fn truncate(key: String) -> String {
        let max_bare = MANAGED_MCP_NAME_MAX_CHARS - MANAGED_MCP_PREFIX.len();
        match key.char_indices().nth(max_bare) {
            Some((i, _)) => key[..i].to_string(),
            None => key,
        }
    }
    let mut pattern_key = key(pattern);
    let mut name_key = key(name);
    // Truncate only for the shape runtime truncation produces (a managed
    // name at exactly the cap), so a long entry never becomes a prefix grant
    // over decoys. Residual: a decoy spelled exactly like the truncated
    // managed name is string-identical — no name matcher can tell them apart.
    if name.starts_with(MANAGED_MCP_PREFIX) && name.chars().count() == MANAGED_MCP_NAME_MAX_CHARS {
        pattern_key = truncate(pattern_key);
        name_key = truncate(name_key);
    }
    !pattern_key.is_empty() && pattern_key == name_key
}

/// Glob options for hosts and DENY paths: case-insensitive (hosts are, and
/// an insensitive deny only over-blocks), `*` spans `/` — path scoping is
/// enforced by the authority/path split, not by the glob.
const POLICY_GLOB_OPTS: glob::MatchOptions = glob::MatchOptions {
    case_sensitive: false,
    require_literal_separator: false,
    require_literal_leading_dot: false,
};

/// Glob options for ALLOW paths: case-sensitive — URL paths name
/// case-sensitive resources, and an allow match is a positive grant, so
/// `/mcp/*` must not grant `/MCP/*`.
const ALLOW_PATH_GLOB_OPTS: glob::MatchOptions = glob::MatchOptions {
    case_sensitive: true,
    require_literal_separator: false,
    require_literal_leading_dot: false,
};

/// Split `authority/path…` at the first `/` into `(authority, path)`.
fn split_authority_path(s: &str) -> (&str, &str) {
    match s.find('/') {
        Some(i) => (&s[..i], &s[i..]),
        None => (s, ""),
    }
}

/// A runtime MCP URL reduced to its connect-time components by the WHATWG
/// parser reqwest uses — hand-parsing diverges from the connect target,
/// which is the gap behind every dodge in this class (`\` authority ends,
/// `%2e%2e` segments, userinfo, default ports, alternate IP spellings).
/// Patterns must NOT use this: they may hold globs the parser rejects.
struct RuntimeUrl {
    scheme: String,
    host: url::Host<String>,
    /// `None` when elided or equal to the scheme default, like the client.
    port: Option<u16>,
    /// WHATWG-normalized: never empty, dot segments resolved, no query.
    path: String,
}

impl RuntimeUrl {
    fn parse(url: &str) -> Option<Self> {
        let parsed = url::Url::parse(url).ok()?;
        let host = parsed.host()?.to_owned();
        Some(Self {
            scheme: parsed.scheme().to_string(),
            host,
            port: parsed.port(),
            path: decode_unreserved_escapes(parsed.path()),
        })
    }

    /// Canonical bare host (lowercase, trailing dot stripped, unbracketed
    /// IPv6) — the same shape [`split_host_path`] yields for patterns.
    fn host_text(&self) -> String {
        match &self.host {
            url::Host::Domain(d) => d.trim_end_matches('.').to_ascii_lowercase(),
            url::Host::Ipv4(a) => a.to_string(),
            url::Host::Ipv6(a) => a.to_string(),
        }
    }

    /// Canonical `host[:port]` for allow matching; IPv6 stays bracketed so it
    /// lines up with `[...]` patterns.
    fn authority(&self) -> String {
        let host = match &self.host {
            url::Host::Ipv6(a) => format!("[{a}]"),
            _ => self.host_text(),
        };
        match self.port {
            Some(port) => format!("{host}:{port}"),
            None => host,
        }
    }
}

/// Glob-match an ALLOW pattern: scheme, host, port, and path matched
/// separately so a wildcard can't cross a component boundary (an allow match
/// is a positive grant under the lockdown). Scheme and port stay literal,
/// except a scheme-default `:443`/`:80` matches the port-less spelling. The
/// URL side is parsed as the client connects ([`RuntimeUrl`]); unparseable =
/// no grant. Deny matching must NOT reuse this — see [`url_deny_matches`].
fn url_allow_matches(pattern: &str, url: &str) -> bool {
    let Some(runtime) = RuntimeUrl::parse(url) else {
        return false;
    };
    let (pat_scheme, pat_rest) = split_scheme(pattern);
    match pat_scheme {
        Some(p) if p.eq_ignore_ascii_case(&runtime.scheme) => {}
        // Scheme-less and glob-scheme patterns never match (warned at parse).
        _ => return false,
    }
    let (pat_authority, pat_path) = split_authority_path(pat_rest);
    // Userinfo in a pattern drops like the connect-time parser drops it —
    // before bracket detection, so `token@[::1]` still reads as IPv6.
    let pat_authority = pat_authority.rsplit('@').next().unwrap_or(pat_authority);
    let url_authority = runtime.authority();
    // A bracketed IPv6 PATTERN isn't a valid glob (`[` opens a class):
    // compare parsed addresses, ports literal, so alternate spellings can't
    // dodge. Only the pattern side chooses this branch — a host glob like
    // `*` must still see IPv6 runtimes or wildcard allows lose those grants.
    // A leading `[` whose content isn't an address opens a glob character
    // class and matches below, like the deny side.
    if is_bracketed_ipv6(pat_authority) {
        let pat_authority = strip_pattern_default_port(pat_authority, &runtime.scheme);
        if !ipv6_authorities_equal(pat_authority, &url_authority) {
            return false;
        }
        return allow_path_matches(&canonicalize_pattern_path(pat_path), &runtime.path);
    }
    // Host and port match separately so a trailing host wildcard can't
    // absorb a port constraint. No pattern port = scheme-default only; an
    // explicit port matches the effective connect port either spelling.
    let (pat_host, pat_port) = match pat_authority.rsplit_once(':') {
        Some((host, port)) => (host, Some(port)),
        None => (pat_authority, None),
    };
    let glob_match = |pat: &str, s: &str| {
        glob::Pattern::new(pat)
            .map(|p| p.matches_with(s, POLICY_GLOB_OPTS))
            .unwrap_or(false)
    };
    if !glob_match(&canonicalize_pattern_host(pat_host), &runtime.host_text()) {
        return false;
    }
    // Ports are literal numbers — a glob or non-numeric port grants nothing
    // (fail closed), and leading zeros compare numerically.
    let port_ok = match (pat_port, runtime.port) {
        (None, None) => true,
        (None, Some(_)) => false,
        (Some(p), Some(port)) => p.parse::<u16>() == Ok(port),
        (Some(p), None) => scheme_default_port(&runtime.scheme)
            .is_some_and(|default| p.parse::<u16>().ok() == default.parse::<u16>().ok()),
    };
    if !port_ok {
        return false;
    }
    allow_path_matches(&canonicalize_pattern_path(pat_path), &runtime.path)
}

/// Canonicalize a pattern host like the WHATWG parser: IDNA/punycode
/// (`bücher.example` → `xn--bcher-kva.example`), percent-decoding
/// (`%61dmin.example` → `admin.example`), lowercase, trailing dot dropped.
/// Per label; glob labels stay literal.
fn canonicalize_pattern_host(host: &str) -> String {
    let host = host.trim_end_matches('.');
    if host.is_ascii() && !host.contains('%') {
        return host.to_ascii_lowercase();
    }
    host.split('.')
        .map(|label| {
            if label.contains(['*', '?', '[']) || (label.is_ascii() && !label.contains('%')) {
                label.to_ascii_lowercase()
            } else {
                match url::Host::parse(label) {
                    Ok(url::Host::Domain(canonical)) => canonical,
                    _ => label.to_ascii_lowercase(),
                }
            }
        })
        .collect::<Vec<_>>()
        .join(".")
}

/// Decode percent escapes of UNRESERVED bytes (RFC 3986 alphanumeric +
/// `-._~`), which the WHATWG parser leaves as-is — otherwise `/%61dmin/x`
/// dodges a `/admin/*` deny. Reserved escapes (`%2F`) stay encoded (decoding
/// them would change the path structure) but normalize to uppercase hex:
/// the parser preserves pre-existing escape case, and allow paths match
/// case-sensitively, so `%2f` and `%2F` must compare equal. Escaped dot
/// segments are already resolved on the runtime side, so a remaining `%2e`
/// can't re-create one.
fn decode_unreserved_escapes(path: &str) -> String {
    fn hex_val(b: u8) -> Option<u8> {
        match b {
            b'0'..=b'9' => Some(b - b'0'),
            b'a'..=b'f' => Some(b - b'a' + 10),
            b'A'..=b'F' => Some(b - b'A' + 10),
            _ => None,
        }
    }
    let bytes = path.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(hi), Some(lo)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2]))
        {
            let decoded = hi * 16 + lo;
            if decoded.is_ascii_alphanumeric() || matches!(decoded, b'-' | b'.' | b'_' | b'~') {
                out.push(decoded);
            } else {
                out.push(b'%');
                out.push(bytes[i + 1].to_ascii_uppercase());
                out.push(bytes[i + 2].to_ascii_uppercase());
            }
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    // Only ASCII bytes were substituted, so the result stays valid UTF-8.
    String::from_utf8_lossy(&out).into_owned()
}

/// Resolve literal `.`/`..` segments in a PATTERN path the way the WHATWG
/// parser resolves the runtime path, so a deny spelled `/x/../admin/*`
/// scopes to `/admin/*` instead of never matching (the runtime side always
/// arrives resolved). `..` clamps at root; a trailing dot segment keeps the
/// directory slash, like the serializer. A glob counts as one segment, so
/// `..` pops it whole.
fn resolve_pattern_dot_segments(path: &str) -> String {
    if !path.contains("/.") {
        return path.to_string();
    }
    let ends_dir = matches!(path.rsplit('/').next(), Some(".") | Some(".."));
    let mut out: Vec<&str> = Vec::new();
    for seg in path.split('/') {
        match seg {
            "." => {}
            ".." => {
                if out.len() > 1 {
                    out.pop();
                }
            }
            s => out.push(s),
        }
    }
    let mut joined = out.join("/");
    if joined.is_empty() {
        joined.push('/');
    }
    if ends_dir && !joined.ends_with('/') {
        joined.push('/');
    }
    joined
}

/// Percent-encode a pattern path the way the WHATWG serializer emits
/// runtime paths, so a Unicode policy path (`/café/*`) matches its connect
/// spelling (`/caf%C3%A9/*`); unreserved escapes decode (kept escapes
/// uppercase, both sides) and dot segments resolve first. `?` stays literal
/// (glob metachar; a runtime path can't contain one).
fn canonicalize_pattern_path(path: &str) -> String {
    let path = resolve_pattern_dot_segments(&decode_unreserved_escapes(path));
    fn needs_encoding(b: u8) -> bool {
        !b.is_ascii()
            || b < 0x20
            || matches!(
                b,
                b' ' | b'"' | b'#' | b'<' | b'>' | b'`' | b'{' | b'}' | 0x7f
            )
    }
    if !path.bytes().any(needs_encoding) {
        return path;
    }
    let hex = |n: u8| {
        char::from_digit(n as u32, 16)
            .unwrap_or('0')
            .to_ascii_uppercase()
    };
    let mut out = String::with_capacity(path.len());
    for b in path.bytes() {
        if needs_encoding(b) {
            out.push('%');
            out.push(hex(b >> 4));
            out.push(hex(b & 0x0f));
        } else {
            out.push(b as char);
        }
    }
    out
}

/// Default port the WHATWG parser elides for a scheme; `None` for unknown
/// schemes (their pattern ports stay literal, failing closed on a mismatch).
fn scheme_default_port(scheme: &str) -> Option<&'static str> {
    match scheme {
        "http" | "ws" => Some("80"),
        "https" | "wss" => Some("443"),
        _ => None,
    }
}

/// Strip an explicit scheme-default port (numerically, so `:0443` counts)
/// from a bracketed-IPv6 pattern authority — the WHATWG parser elides it on
/// the URL side, and both spellings name the same connect target. The `]`
/// guard keeps a port-less address ending in a default-port hextet intact.
fn strip_pattern_default_port<'a>(authority: &'a str, scheme: &str) -> &'a str {
    let Some(default) = scheme_default_port(scheme).and_then(|d| d.parse::<u16>().ok()) else {
        return authority;
    };
    match authority.rsplit_once(':') {
        Some((host, port)) if host.ends_with(']') && port.parse::<u16>() == Ok(default) => host,
        _ => authority,
    }
}

/// Allow-side path match: an authority-only pattern matches only the root
/// path; `/*` spans nested segments. The URL path arrives WHATWG-normalized,
/// so the glob sees what the server will actually receive.
fn allow_path_matches(pat_path: &str, url_path: &str) -> bool {
    if pat_path.is_empty() || pat_path == "/" {
        return url_path == "/";
    }
    glob::Pattern::new(pat_path)
        .map(|p| p.matches_with(url_path, ALLOW_PATH_GLOB_OPTS))
        .unwrap_or(false)
}

/// True when a pattern authority is a bracketed IPv6 literal, `[v6]` or
/// `[v6]:port`. A leading `[` whose content doesn't parse as an address
/// opens a glob character class instead (`[ab]host.example`).
fn is_bracketed_ipv6(authority: &str) -> bool {
    authority
        .strip_prefix('[')
        .and_then(|rest| rest.split_once(']'))
        .is_some_and(|(addr, _)| addr.parse::<std::net::Ipv6Addr>().is_ok())
}

/// Compare two bracketed-IPv6 authorities by parsed address (ports numeric
/// when both parse, so `:0443` ≡ `:443`), so alternate spellings of one
/// address (`[2001:0db8::1]`, `[2001:db8:0:0:0:0:0:1]`) can't dodge a
/// policy entry. When either side isn't a parseable bracketed literal, fall
/// back to the literal comparison (an imprecise allow only over-blocks).
fn ipv6_authorities_equal(a: &str, b: &str) -> bool {
    fn parse(authority: &str) -> Option<(std::net::Ipv6Addr, &str)> {
        let rest = authority.strip_prefix('[')?;
        let end = rest.find(']')?;
        let addr: std::net::Ipv6Addr = rest[..end].parse().ok()?;
        Some((addr, &rest[end + 1..]))
    }
    fn ports_equal(a: &str, b: &str) -> bool {
        match (
            a.strip_prefix(':').map(str::parse::<u16>),
            b.strip_prefix(':').map(str::parse::<u16>),
        ) {
            (Some(Ok(pa)), Some(Ok(pb))) => pa == pb,
            _ => a == b,
        }
    }
    match (parse(a), parse(b)) {
        (Some((addr_a, port_a)), Some((addr_b, port_b))) => {
            addr_a == addr_b && ports_equal(port_a, port_b)
        }
        _ => a.eq_ignore_ascii_case(b),
    }
}

/// Split `scheme://rest` into `(Some(scheme), rest)`, or `(None, s)`.
fn split_scheme(s: &str) -> (Option<&str>, &str) {
    match s.find("://") {
        Some(i) => (Some(&s[..i]), &s[i + 3..]),
        None => (None, s),
    }
}

/// Host-normalized, scheme/port-agnostic match of a DENY pattern against a
/// URL. Deliberately asymmetric with [`url_allow_matches`]: an imprecise
/// allow only over-blocks (safe), but a deny must never fail open — so
/// scheme and port are ignored and a `host` / `scheme://host/*` entry blocks
/// that host on ANY scheme, port, and path. A URL [`RuntimeUrl`] rejects is
/// denied outright: a spelling we can't interpret must not slip past.
fn url_deny_matches(pattern: &str, url: &str) -> bool {
    let (Some(pat_host), pat_path) = split_host_path(pattern) else {
        return false;
    };
    let Some(runtime) = RuntimeUrl::parse(url) else {
        return true;
    };
    let url_host = runtime.host_text();
    // An invalid deny HOST glob still denies by literal comparison.
    let glob_match = |pat: &str, s: &str| match glob::Pattern::new(pat) {
        Ok(p) => p.matches_with(s, POLICY_GLOB_OPTS),
        Err(_) => pat.eq_ignore_ascii_case(s),
    };
    // IP-named entries compare parsed addresses so alternate spellings of
    // one connect target (`0xa9fea9fe`, `127.1`) are still denied; globs
    // never parse as an IP and fall through.
    if let (Some(pat_ip), Some(url_ip)) = (parse_ip_host(&pat_host), parse_ip_host(&url_host)) {
        if pat_ip != url_ip {
            return false;
        }
    } else if !glob_match(&canonicalize_pattern_host(&pat_host), &url_host) {
        return false;
    }
    // A host-only pattern blocks every path on that host — including every
    // spelling whose CANONICAL path is `/` (`https://host/`, `/.`,
    // `/mcp/..`), all WHATWG-equal to the pathless form. Otherwise glob the
    // canonical pattern path against the WHATWG-normalized URL path, so
    // `/admin/*` can't be dodged by spelling the URL `/mcp/../admin/x`.
    let pat_path = canonicalize_pattern_path(&pat_path);
    if pat_path.is_empty() || pat_path == "/" {
        return true;
    }
    match glob::Pattern::new(&pat_path) {
        Ok(p) => p.matches_with(&runtime.path, POLICY_GLOB_OPTS),
        // Host matched + broken path glob: deny the whole host, never
        // fail open.
        Err(e) => {
            warn!(
                pattern,
                error = %e,
                "invalid deniedMcpServers path glob; denying every path on the matched host"
            );
            true
        }
    }
}

/// Parse a bare host as an IP, accepting every spelling the WHATWG parser
/// canonicalizes at connect time (hex, shortened, decimal, unbracketed
/// IPv6). IPv4-mapped IPv6 canonicalizes to the IPv4 it reaches on a
/// dual-stack socket. Domains and glob patterns return `None`.
fn parse_ip_host(host: &str) -> Option<std::net::IpAddr> {
    let addr = if let Ok(v6) = host.parse::<std::net::Ipv6Addr>() {
        std::net::IpAddr::V6(v6)
    } else {
        match url::Host::parse(host) {
            Ok(url::Host::Ipv4(a)) => std::net::IpAddr::V4(a),
            Ok(url::Host::Ipv6(a)) => std::net::IpAddr::V6(a),
            _ => return None,
        }
    };
    Some(addr.to_canonical())
}

/// Split a URL PATTERN into `(host, path)`, dropping scheme, userinfo, and
/// port. The host is lowercased with a trailing dot stripped; the path keeps
/// its original case and any glob metacharacters. Runtime URLs must be parsed
/// with [`RuntimeUrl`] instead — hand-splitting a runtime URL diverges from
/// the connect target.
fn split_host_path(s: &str) -> (Option<String>, String) {
    let after_scheme = match s.find("://") {
        Some(i) => &s[i + 3..],
        None => s,
    };
    let (authority, path) = match after_scheme.find('/') {
        Some(i) => (&after_scheme[..i], &after_scheme[i..]),
        None => (after_scheme, ""),
    };
    // Drop userinfo then the port; IPv6 keeps its colons whether bracketed
    // or not (a naive `:`-split truncates `2001:db8::1` to `2001`). A
    // decimal suffix is also a valid hextet; the `host:port` reading wins
    // that ambiguity (`…::1:443` is a copied URL's host + port, and reading
    // it as an address under-blocks the intended host). `:ffff` is a hextet.
    // A leading `[` only means IPv6 when its content parses as one —
    // otherwise it opens a glob character class (`[ab]evil.example`).
    let authority = authority.rsplit('@').next().unwrap_or(authority);
    let bracket_ipv6 = authority.strip_prefix('[').and_then(|rest| {
        let content = match rest.find(']') {
            Some(i) => &rest[..i],
            None => rest,
        };
        content
            .parse::<std::net::Ipv6Addr>()
            .is_ok()
            .then_some(content)
    });
    let host = if let Some(content) = bracket_ipv6 {
        content
    } else if let Some((before_port, port)) = authority.rsplit_once(':')
        && !port.is_empty()
        && port.bytes().all(|b| b.is_ascii_digit())
        && port.parse::<u16>().is_ok()
        && before_port.parse::<std::net::Ipv6Addr>().is_ok()
    {
        before_port
    } else if authority.parse::<std::net::Ipv6Addr>().is_ok() {
        authority
    } else {
        authority.split(':').next().unwrap_or(authority)
    };
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    if host.is_empty() {
        (None, path.to_string())
    } else {
        (Some(host), path.to_string())
    }
}

/// When non-empty, only git marketplace sources matching an allowed URL are
/// permitted.
#[derive(Debug, Clone, Default)]
pub struct MarketplaceAllowlist {
    pub allowed_urls: Vec<String>,
    pub source_path: Option<std::path::PathBuf>,
}

impl MarketplaceAllowlist {
    pub fn is_restricted(&self) -> bool {
        !self.allowed_urls.is_empty()
    }

    pub fn is_url_allowed(&self, url: &str) -> bool {
        if self.allowed_urls.is_empty() {
            return true;
        }
        let normalized = normalize_git_url(url);
        self.allowed_urls
            .iter()
            .any(|allowed| normalize_git_url(allowed) == normalized)
    }

    pub fn block_reason(&self) -> String {
        match &self.source_path {
            Some(p) => format!("source not in strictKnownMarketplaces ({})", p.display()),
            None => "source not in strictKnownMarketplaces".to_string(),
        }
    }
}

fn normalize_git_url(url: &str) -> String {
    url.to_lowercase().trim_end_matches(".git").to_string()
}

#[cfg(test)]
#[path = "resolution_tests.rs"]
mod tests;
