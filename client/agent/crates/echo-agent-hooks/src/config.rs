use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::HookError;
use crate::event::HookEventName;
use crate::matcher::HookMatcher;

pub use echo_agent_config::HookProvenance;

/// Parsed `hooks` object. Unknown event names are skipped, not errors.
#[derive(Debug)]
pub struct HooksMap {
    pub events: HashMap<HookEventName, Vec<MatcherGroup>>,
    pub skipped_events: Vec<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum GroupErrorPolicy {
    Fail,
    SkipEvent,
}

impl HooksMap {
    fn assemble<V>(
        entries: HashMap<String, V>,
        mut parse_groups: impl FnMut(V) -> Result<Vec<MatcherGroup>, String>,
        group_errors: GroupErrorPolicy,
    ) -> Result<Self, String> {
        let mut events: HashMap<HookEventName, Vec<MatcherGroup>> = HashMap::new();
        let mut skipped_events = Vec::new();

        for (key, val) in entries {
            let event_name = match HookEventName::parse_key(&key) {
                Some(name) => name,
                None => {
                    skipped_events.push(key);
                    continue;
                }
            };

            match parse_groups(val) {
                Ok(groups) => events.entry(event_name).or_default().extend(groups),
                Err(detail) => match group_errors {
                    GroupErrorPolicy::Fail => {
                        return Err(format!(
                            "invalid matcher groups for event '{key}': {detail}"
                        ));
                    }
                    GroupErrorPolicy::SkipEvent => {
                        tracing::warn!(
                            event = %key,
                            error = %detail,
                            "hooks: skipping malformed event in config layer (other events still load)"
                        );
                        skipped_events.push(key);
                    }
                },
            }
        }

        Ok(HooksMap {
            events,
            skipped_events,
        })
    }

    /// Parse a `hooks` object from JSON. A malformed event fails the whole parse.
    pub fn from_value(value: serde_json::Value) -> Result<Self, String> {
        let entries: HashMap<String, serde_json::Value> =
            serde_json::from_value(value).map_err(|e| format!("invalid hooks structure: {e}"))?;
        Self::assemble(
            entries,
            |v| serde_json::from_value(v).map_err(|e| e.to_string()),
            GroupErrorPolicy::Fail,
        )
    }

    /// Parse a `hooks` table from TOML. Unlike [`Self::from_value`], a malformed
    /// event is skipped so one bad event can't drop the layer.
    pub fn from_toml_value(value: toml::Value) -> Result<Self, String> {
        let entries: HashMap<String, toml::Value> = value
            .try_into()
            .map_err(|e: toml::de::Error| format!("invalid hooks structure: {e}"))?;
        Self::assemble(
            entries,
            |v| v.try_into().map_err(|e: toml::de::Error| e.to_string()),
            GroupErrorPolicy::SkipEvent,
        )
    }
}

#[derive(Debug, Deserialize)]
pub struct MatcherGroup {
    #[serde(default)]
    pub matcher: Option<String>,
    pub hooks: Vec<RawHandler>,
}

#[derive(Debug, Deserialize)]
pub struct RawHandler {
    #[serde(rename = "type")]
    pub handler_type: String,
    pub command: Option<String>,
    pub url: Option<String>,
    /// Seconds (converted to milliseconds internally).
    pub timeout: Option<u64>,
    /// Extra env vars, merged into [`HookSpec::extra_env`].
    #[serde(default, deserialize_with = "deserialize_optional_string_map")]
    pub env: HashMap<String, String>,
}

/// Treat `null` or an absent field as an empty map (serde otherwise rejects
/// `null` for a `HashMap`).
fn deserialize_optional_string_map<'de, D>(de: D) -> Result<HashMap<String, String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let opt: Option<HashMap<String, String>> = serde::Deserialize::deserialize(de)?;
    Ok(opt.unwrap_or_default())
}

pub const DEFAULT_TIMEOUT_SECS: u64 = 5;

pub const DEFAULT_TIMEOUT_MS: u64 = DEFAULT_TIMEOUT_SECS * 1000;

/// Stop gates run real verification (builds, tests) and fail open on timeout, so
/// the short observe default would silently disable a ported stop policy.
pub const DEFAULT_STOP_GATE_TIMEOUT_SECS: u64 = 600;

pub const DEFAULT_STOP_GATE_TIMEOUT_MS: u64 = DEFAULT_STOP_GATE_TIMEOUT_SECS * 1000;

fn default_timeout_ms(event: crate::event::HookEventName) -> u64 {
    if event.traits().gate == crate::event::GateKind::Stop {
        DEFAULT_STOP_GATE_TIMEOUT_MS
    } else {
        DEFAULT_TIMEOUT_MS
    }
}

/// The validated handler kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HandlerType {
    Command,
    Http,
}

impl HandlerType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Command => "command",
            Self::Http => "http",
        }
    }
}

impl std::str::FromStr for HandlerType {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "command" => Ok(Self::Command),
            "http" => Ok(Self::Http),
            _ => Err(()),
        }
    }
}

/// A validated hook specification, ready for the dispatcher.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookSpec {
    pub name: String,
    pub event: HookEventName,
    pub handler_type: HandlerType,
    /// Pattern as written; the compiled form is `matcher`.
    pub configured_matcher: Option<String>,
    #[serde(skip)]
    pub matcher: Option<HookMatcher>,
    pub enabled: bool,
    /// Command path, env-expanded; unresolved/modifier forms kept for the runner's
    /// `sh -c` branch. Not re-expanded at run time. Display via `command_raw`.
    pub command: Option<PathBuf>,
    /// Pre-expansion `command` for display, so resolved secrets never leak.
    pub command_raw: Option<String>,
    /// URL (http handlers), env-expanded. Unlike `command`, the HTTP runner
    /// re-expands at run time before SSRF validation (deliberate asymmetry).
    pub url: Option<String>,
    /// Pre-expansion `url` for display; see `command_raw`.
    pub url_raw: Option<String>,
    pub timeout_ms: u64,
    pub source_dir: PathBuf,
    /// Env injected into the hook process, and consulted by load-time `command`/
    /// `url` expansion. Precedence low→high: user `env` (reserved keys stripped) <
    /// plugin-injected < runner-injected at spawn (authentic identity always wins).
    pub extra_env: std::collections::HashMap<String, String>,
    /// The hook's origin and single source of truth for classification: `File`
    /// (JSON files, agent frontmatter), a config tier, or `Plugin`. `#[serde(default)]`
    /// reads pre-field wire specs as `File`.
    #[serde(default)]
    pub layer: HookProvenance,
}

pub const RUNNER_ALWAYS_SET_ENV: &[&str] = &[
    "ECHO_AGENT_HOOK_EVENT",
    "ECHO_AGENT_HOOK_NAME",
    "ECHO_AGENT_SESSION_ID",
    "ECHO_AGENT_WORKSPACE_ROOT",
    "CLAUDE_PROJECT_DIR",
];

pub fn expand_env_skipping_runner_vars(input: &str) -> String {
    crate::env_expand::expand_env_vars_with_process_skip(
        input,
        &HashMap::new(),
        RUNNER_ALWAYS_SET_ENV,
    )
}

/// Namespace prefixes stamped on hook names, matched by [`hook_origin`]. Shared
/// so a rename can't silently reclassify a tier.
pub const GLOBAL_HOOK_PREFIX: &str = "global/";
pub const PROJECT_HOOK_PREFIX: &str = "project/";
pub const PLUGIN_HOOK_PREFIX: &str = "plugin/";
pub const AGENT_HOOK_PREFIX: &str = "agent:";

/// A hook's classified origin for display and telemetry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookOrigin {
    SystemManaged,
    Managed,
    Requirements,
    UserConfig,
    UserFile,
    ProjectFile,
    Plugin,
    Agent,
    Unknown,
}

/// Classify a hook's origin from [`HookProvenance`], falling back to the name
/// prefix for `File`-tier hooks.
pub fn hook_origin(spec: &HookSpec) -> HookOrigin {
    match spec.layer {
        HookProvenance::SystemManaged => HookOrigin::SystemManaged,
        HookProvenance::Managed => HookOrigin::Managed,
        HookProvenance::Requirements => HookOrigin::Requirements,
        HookProvenance::User => HookOrigin::UserConfig,
        HookProvenance::Plugin => HookOrigin::Plugin,
        HookProvenance::Unknown => HookOrigin::Unknown,
        HookProvenance::File => {
            let name = spec.name.as_str();
            if name.starts_with(GLOBAL_HOOK_PREFIX) {
                HookOrigin::UserFile
            } else if name.starts_with(PROJECT_HOOK_PREFIX) {
                HookOrigin::ProjectFile
            } else if name.starts_with(AGENT_HOOK_PREFIX) {
                HookOrigin::Agent
            } else if name.starts_with(PLUGIN_HOOK_PREFIX) {
                // Defensive: a plugin hook whose adapter didn't stamp `layer`.
                HookOrigin::Plugin
            } else {
                HookOrigin::Unknown
            }
        }
    }
}

/// Parse hooks from a JSON value (e.g. from agent definition frontmatter).
///
/// `source_dir` resolves relative command paths: pass the agent definition's
/// directory or the workspace CWD.
pub fn parse_hooks_from_value(
    hooks: &serde_json::Value,
    source_name: &str,
) -> (Vec<HookSpec>, Vec<HookError>) {
    parse_hooks_from_value_with_dir(hooks, source_name, std::path::Path::new("."))
}

/// [`parse_hooks_from_value`] with an explicit `source_dir`. Parses the decoded
/// value directly (no re-parse round-trip); a malformed event is a hard error.
pub fn parse_hooks_from_value_with_dir(
    hooks: &serde_json::Value,
    source_name: &str,
    source_dir: &Path,
) -> (Vec<HookSpec>, Vec<HookError>) {
    let error_path = Path::new(source_name);
    let hooks_map = match HooksMap::from_value(hooks.clone()) {
        Ok(map) => map,
        Err(detail) => {
            return (
                Vec::new(),
                vec![HookError::ParseFile {
                    path: error_path.to_path_buf(),
                    detail,
                }],
            );
        }
    };
    if !hooks_map.skipped_events.is_empty() {
        tracing::warn!(
            source = %source_name,
            skipped = ?hooks_map.skipped_events,
            "hooks: skipped unrecognized event names (check for typos)"
        );
    }

    let name_prefix = error_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown");
    build_specs(
        hooks_map,
        SpecContext {
            name_prefix,
            source_dir,
            error_path,
            provenance: HookProvenance::File,
        },
    )
}

/// Build specs from config-layer `hooks` blocks, tagging each with its layer's
/// `source_name`. Layers arrive highest-authority-first and specs preserve that
/// order, so the caller's dedup keeps the higher-authority copy. Relative commands
/// resolve against each layer's own directory; a layer that fails to parse is
/// recorded and skipped, the rest still load.
pub fn parse_hooks_from_config_layers(
    layers: &[echo_agent_config::HookConfigLayer],
) -> (Vec<HookSpec>, Vec<HookError>) {
    let home = echo_agent_config::user_echo_agent_home();
    let mut all_specs = Vec::new();
    let mut all_errors = Vec::new();

    for layer in layers {
        let source_name = layer.source_name();
        let error_path = layer.path();
        // Resolve relative commands against the layer's own dir, not the user home.
        let source_dir = match error_path.parent() {
            Some(dir) if !dir.as_os_str().is_empty() => dir.to_path_buf(),
            _ => home.clone().unwrap_or_else(|| PathBuf::from(".")),
        };
        let hooks_map = match HooksMap::from_toml_value(layer.hooks().clone()) {
            Ok(map) => map,
            Err(detail) => {
                all_errors.push(HookError::ParseFile {
                    path: error_path.to_path_buf(),
                    detail,
                });
                continue;
            }
        };
        if !hooks_map.skipped_events.is_empty() {
            tracing::warn!(
                source = %source_name,
                skipped = ?hooks_map.skipped_events,
                "hooks: skipped unrecognized or malformed events in config layer"
            );
        }
        let (specs, errors) = build_specs(
            hooks_map,
            SpecContext {
                name_prefix: source_name,
                source_dir: &source_dir,
                error_path,
                provenance: layer.provenance(),
            },
        );
        all_specs.extend(specs);
        all_errors.extend(errors);
    }

    (all_specs, all_errors)
}

pub fn parse_hook_file(content: &str, file_path: &Path) -> (Vec<HookSpec>, Vec<HookError>) {
    let specs = Vec::new();
    let mut errors = Vec::new();

    let top_level: serde_json::Value = match serde_json::from_str(content) {
        Ok(v) => v,
        Err(e) => {
            errors.push(HookError::ParseFile {
                path: file_path.to_path_buf(),
                detail: e.to_string(),
            });
            return (specs, errors);
        }
    };

    let hooks_value = match top_level.get("hooks") {
        Some(v) => v.clone(),
        None => return (specs, errors),
    };

    let hooks_map: HooksMap = match HooksMap::from_value(hooks_value) {
        Ok(m) => m,
        Err(detail) => {
            errors.push(HookError::ParseFile {
                path: file_path.to_path_buf(),
                detail,
            });
            return (specs, errors);
        }
    };

    if !hooks_map.skipped_events.is_empty() {
        tracing::warn!(
            file = %file_path.display(),
            skipped = ?hooks_map.skipped_events,
            "hooks: skipped unrecognized event names (check for typos)"
        );
    }

    let source_dir = file_path.parent().unwrap_or(Path::new(".")).to_path_buf();
    let file_stem = file_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown");

    build_specs(
        hooks_map,
        SpecContext {
            name_prefix: file_stem,
            source_dir: &source_dir,
            error_path: file_path,
            provenance: HookProvenance::File,
        },
    )
}

/// Build [`HookSpec`]s from a [`HooksMap`], shared by the JSON and config paths
/// so the two never diverge.
fn build_specs(hooks_map: HooksMap, ctx: SpecContext<'_>) -> (Vec<HookSpec>, Vec<HookError>) {
    let mut specs = Vec::new();
    let mut errors = Vec::new();

    // Stable event order for reproducible output; source order kept within an event.
    let mut events: Vec<(HookEventName, Vec<MatcherGroup>)> =
        hooks_map.events.into_iter().collect();
    events.sort_by_key(|(event, _)| *event);
    for (event, matcher_groups) in events {
        for (group_idx, group) in matcher_groups.into_iter().enumerate() {
            let group_label = format!("{}:{event}[{group_idx}]", ctx.name_prefix);
            let (configured_matcher, compiled_matcher) =
                match resolve_group_matcher(group.matcher.as_deref(), event, &group_label, &ctx) {
                    Ok(pair) => pair,
                    Err(e) => {
                        errors.push(e);
                        continue;
                    }
                };

            for (hook_idx, handler) in group.hooks.into_iter().enumerate() {
                let name = format!("{group_label}.hooks[{hook_idx}]");
                match build_one_spec(
                    handler,
                    event,
                    name,
                    configured_matcher.clone(),
                    compiled_matcher.clone(),
                    &ctx,
                ) {
                    Ok(spec) => specs.push(spec),
                    Err(e) => errors.push(e),
                }
            }
        }
    }

    (specs, errors)
}

/// Resolve a group's `(configured_matcher, compiled_matcher)`. The compiled
/// matcher is `None` with no pattern, or when the event ignores matchers (pattern
/// kept for display, hook always fires). Errors only on an invalid regex.
fn resolve_group_matcher(
    group_matcher: Option<&str>,
    event: HookEventName,
    group_label: &str,
    ctx: &SpecContext<'_>,
) -> Result<(Option<String>, Option<HookMatcher>), HookError> {
    let configured = group_matcher
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());

    if configured.is_some() && event.traits().matcher == crate::event::MatcherPolicy::Ignored {
        tracing::warn!(
            hook = %group_label,
            path = %ctx.error_path.display(),
            "hooks: matcher on a {event} group is ignored (this event always fires)"
        );
        return Ok((configured, None));
    }

    let compiled = match configured.as_deref() {
        Some(pattern) => {
            Some(
                HookMatcher::new(pattern).map_err(|source| HookError::InvalidMatcher {
                    name: group_label.to_string(),
                    path: ctx.error_path.to_path_buf(),
                    source,
                })?,
            )
        }
        None => None,
    };
    Ok((configured, compiled))
}

/// Per-call constants shared by every group and handler in one [`build_specs`].
struct SpecContext<'a> {
    /// Labels specs as `"{name_prefix}:{event}[..]"` (file stem or config `source_name`).
    name_prefix: &'a str,
    source_dir: &'a Path,
    error_path: &'a Path,
    provenance: HookProvenance,
}

/// Build one [`HookSpec`] from a handler entry, or the [`HookError`] preventing it.
/// `command`/`url` are env-expanded (unset refs kept for the runner); `matcher` is
/// not, since `$` is the regex end anchor.
fn build_one_spec(
    handler: RawHandler,
    event: HookEventName,
    name: String,
    configured_matcher: Option<String>,
    compiled_matcher: Option<HookMatcher>,
    ctx: &SpecContext<'_>,
) -> Result<HookSpec, HookError> {
    let timeout_ms = handler
        .timeout
        // Untrusted config value: saturate rather than overflow (debug panic /
        // release wrap) on an absurd timeout.
        .map(|secs| secs.saturating_mul(1000))
        .unwrap_or(default_timeout_ms(event));

    let mut extra_env: HashMap<String, String> = handler.env;
    strip_reserved_env_keys(&mut extra_env, &name, ctx.error_path);

    let handler_type = match handler.handler_type.parse::<HandlerType>() {
        Ok(ht) => ht,
        Err(()) => {
            return Err(HookError::UnsupportedHandlerType {
                name,
                path: ctx.error_path.to_path_buf(),
                handler_type: handler.handler_type,
            });
        }
    };

    let (command, command_raw, url, url_raw) = match handler_type {
        HandlerType::Command => {
            let Some(command) = handler.command else {
                return Err(HookError::InvalidConfig {
                    name,
                    path: ctx.error_path.to_path_buf(),
                    detail: "command handler requires a 'command' field".into(),
                });
            };
            let expanded = crate::env_expand::expand_env_vars_with_process_skip(
                &command,
                &extra_env,
                RUNNER_ALWAYS_SET_ENV,
            );
            (Some(PathBuf::from(expanded)), Some(command), None, None)
        }
        HandlerType::Http => {
            let Some(url) = handler.url else {
                return Err(HookError::InvalidConfig {
                    name,
                    path: ctx.error_path.to_path_buf(),
                    detail: "http handler requires a 'url' field".into(),
                });
            };
            let expanded = crate::env_expand::expand_env_vars_with_process_skip(
                &url,
                &extra_env,
                RUNNER_ALWAYS_SET_ENV,
            );
            (None, None, Some(expanded), Some(url))
        }
    };

    Ok(HookSpec {
        name,
        event,
        handler_type,
        configured_matcher,
        matcher: compiled_matcher,
        enabled: true,
        command,
        command_raw,
        url,
        url_raw,
        timeout_ms,
        source_dir: ctx.source_dir.to_path_buf(),
        extra_env,
        layer: ctx.provenance,
    })
}

/// Strip user `env` entries that would shadow runner-reserved keys, with a warning.
fn strip_reserved_env_keys(
    extra_env: &mut HashMap<String, String>,
    spec_name: &str,
    file_path: &Path,
) {
    for reserved in RUNNER_ALWAYS_SET_ENV {
        if extra_env.remove(*reserved).is_some() {
            tracing::warn!(
                hook = %spec_name,
                file = %file_path.display(),
                key = reserved,
                "hook env: ignoring user-supplied value for runner-reserved key (the runner-injected value always wins)"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::with_env_var;

    fn config_layer(source_name: &str, toml_src: &str) -> echo_agent_config::HookConfigLayer {
        let value: toml::Value = toml::from_str(toml_src).unwrap();
        let hooks = value.get("hooks").cloned().unwrap();
        echo_agent_config::HookConfigLayer::new(
            echo_agent_config::HookProvenance::Managed,
            source_name,
            hooks,
        )
    }

    #[test]
    fn config_layer_hook_parses_like_the_json_path() {
        let layer = config_layer(
            "managed",
            "[[hooks.PreToolUse]]\nmatcher = \"Bash\"\n[[hooks.PreToolUse.hooks]]\ntype = \"command\"\ncommand = \"bin/check.sh\"\ntimeout = 2\n",
        );
        let (specs, errors) = parse_hooks_from_config_layers(std::slice::from_ref(&layer));
        assert!(errors.is_empty(), "unexpected errors: {errors:?}");
        assert_eq!(specs.len(), 1);
        let s = &specs[0];
        assert_eq!(s.event, HookEventName::PreToolUse);
        assert_eq!(s.handler_type, HandlerType::Command);
        assert_eq!(s.timeout_ms, 2000);
        assert_eq!(s.layer, HookProvenance::Managed);
        assert!(s.name.starts_with("managed:"), "got {}", s.name);
    }

    #[test]
    fn config_layer_keeps_valid_events_when_one_is_malformed() {
        // A config layer skips a malformed event and keeps the rest, unlike the
        // JSON path which fails the whole file.
        let layer = config_layer(
            "managed",
            "hooks.PreToolUse = \"oops\"\n[[hooks.PostToolUse]]\n[[hooks.PostToolUse.hooks]]\ntype = \"command\"\ncommand = \"ok.sh\"\n",
        );
        let (specs, _errors) = parse_hooks_from_config_layers(std::slice::from_ref(&layer));
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].event, HookEventName::PostToolUse);
    }

    #[test]
    fn config_layers_additive_and_dedup_keeps_higher_authority() {
        let mk = |src: &str, prov, cmd: &str| {
            let toml_src = format!(
                "[[PreToolUse]]\n[[PreToolUse.hooks]]\ntype = \"command\"\ncommand = \"{cmd}\"\n"
            );
            echo_agent_config::HookConfigLayer::new(
                prov,
                src,
                toml::from_str::<toml::Value>(&toml_src).unwrap(),
            )
        };

        // Distinct commands are additive; an identical command dedupes to the
        // higher-authority (first-listed) copy.
        use echo_agent_config::HookProvenance::{Managed, User};
        let (additive, _) = parse_hooks_from_config_layers(&[
            mk("managed", Managed, "m.sh"),
            mk("user", User, "u.sh"),
        ]);
        assert_eq!(additive.len(), 2);

        let (dup, _) = parse_hooks_from_config_layers(&[
            mk("managed", Managed, "same.sh"),
            mk("user", User, "same.sh"),
        ]);
        let registry = crate::discovery::registry_from_specs_deduped(dup);
        let pre = registry.hooks_for(HookEventName::PreToolUse);
        assert_eq!(pre.len(), 1);
        assert!(pre[0].name.starts_with("managed:"), "got {}", pre[0].name);
    }

    #[test]
    fn parse_claude_format_single_hook() {
        let json = r#"{
            "hooks": {
                "PreToolUse": [
                    {
                        "matcher": "run_terminal_cmd",
                        "hooks": [
                            { "type": "command", "command": "bin/check.sh", "timeout": 2 }
                        ]
                    }
                ]
            }
        }"#;
        let (specs, errors) = parse_hook_file(json, Path::new("/tmp/hooks/test.json"));
        assert!(errors.is_empty(), "unexpected errors: {errors:?}");
        assert_eq!(specs.len(), 1);
        let s = &specs[0];
        assert_eq!(s.event, HookEventName::PreToolUse);
        assert!(s.matcher.is_some());
        assert!(s.enabled);
        assert_eq!(s.timeout_ms, 2000);
        assert_eq!(s.command, Some(PathBuf::from("bin/check.sh")));
    }

    #[test]
    fn parse_multiple_handlers_in_group() {
        let json = r#"{
            "hooks": {
                "PreToolUse": [
                    {
                        "matcher": "Bash",
                        "hooks": [
                            { "type": "command", "command": "a.sh" },
                            { "type": "command", "command": "b.sh" }
                        ]
                    }
                ]
            }
        }"#;
        let (specs, errors) = parse_hook_file(json, Path::new("/tmp/test.json"));
        assert!(errors.is_empty());
        assert_eq!(specs.len(), 2);
        assert_eq!(specs[0].command, Some(PathBuf::from("a.sh")));
        assert_eq!(specs[1].command, Some(PathBuf::from("b.sh")));
    }

    #[test]
    fn parse_empty_matcher_matches_all() {
        let json = r#"{
            "hooks": {
                "PreToolUse": [
                    { "matcher": "", "hooks": [{ "type": "command", "command": "a.sh" }] }
                ]
            }
        }"#;
        let (specs, errors) = parse_hook_file(json, Path::new("/tmp/test.json"));
        assert!(errors.is_empty());
        assert!(specs[0].matcher.is_none());
    }

    #[test]
    fn parse_absent_matcher_matches_all() {
        let json = r#"{
            "hooks": {
                "SessionStart": [
                    { "hooks": [{ "type": "command", "command": "start.sh" }] }
                ]
            }
        }"#;
        let (specs, errors) = parse_hook_file(json, Path::new("/tmp/test.json"));
        assert!(errors.is_empty());
        assert!(specs[0].matcher.is_none());
    }

    #[test]
    fn parse_default_timeout() {
        let json = r#"{
            "hooks": {
                "SessionEnd": [
                    { "hooks": [{ "type": "command", "command": "end.sh" }] }
                ],
                "Stop": [
                    { "hooks": [{ "type": "command", "command": "verify.sh" }] }
                ],
                "SubagentStop": [
                    { "hooks": [{ "type": "command", "command": "sub.sh" }] }
                ]
            }
        }"#;
        let (specs, errors) = parse_hook_file(json, Path::new("/tmp/test.json"));
        assert!(errors.is_empty());
        for spec in &specs {
            let expected = match spec.event {
                HookEventName::Stop | HookEventName::SubagentStop => DEFAULT_STOP_GATE_TIMEOUT_MS,
                _ => DEFAULT_TIMEOUT_MS,
            };
            assert_eq!(spec.timeout_ms, expected, "event {}", spec.event);
        }
    }

    #[test]
    fn session_start_matcher_compiles_and_tests_source() {
        let json = r#"{
            "hooks": {
                "SessionStart": [
                    { "matcher": "startup|resume", "hooks": [{ "type": "command", "command": "s.sh" }] }
                ]
            }
        }"#;
        let (specs, errors) = parse_hook_file(json, Path::new("/tmp/test.json"));
        assert!(errors.is_empty(), "unexpected errors: {errors:?}");
        assert_eq!(specs.len(), 1);
        let matcher = specs[0].matcher.as_ref().expect("matcher compiles");
        assert!(matcher.is_match("startup"));
        assert!(!matcher.is_match("clear"));
    }

    #[test]
    fn alias_event_keys_merge_groups() {
        let json = r#"{
            "hooks": {
                "Stop": [
                    { "hooks": [{ "type": "command", "command": "a.sh" }] }
                ],
                "stop": [
                    { "hooks": [{ "type": "command", "command": "b.sh" }] }
                ]
            }
        }"#;
        let (specs, errors) = parse_hook_file(json, Path::new("/tmp/test.json"));
        assert!(errors.is_empty());
        assert_eq!(specs.len(), 2, "both groups must survive the key collision");
    }

    #[test]
    fn stop_matcher_ignored_subagent_stop_matcher_kept() {
        let json = r#"{
            "hooks": {
                "Stop": [
                    { "matcher": "*", "hooks": [{ "type": "command", "command": "s.sh" }] }
                ],
                "SubagentStop": [
                    { "matcher": "code-reviewer", "hooks": [{ "type": "command", "command": "r.sh" }] }
                ]
            }
        }"#;
        let (specs, errors) = parse_hook_file(json, Path::new("/tmp/test.json"));
        assert!(errors.is_empty(), "no load errors expected: {errors:?}");
        assert_eq!(specs.len(), 2);

        let stop = specs
            .iter()
            .find(|s| s.command_raw.as_deref() == Some("s.sh"))
            .unwrap();
        assert!(stop.matcher.is_none(), "Stop matcher must not compile");
        assert_eq!(
            stop.configured_matcher.as_deref(),
            Some("*"),
            "the configured pattern stays visible for display"
        );

        let sub = specs
            .iter()
            .find(|s| s.command_raw.as_deref() == Some("r.sh"))
            .unwrap();
        assert!(
            sub.matcher
                .as_ref()
                .is_some_and(|m| m.is_match("code-reviewer")),
            "SubagentStop matcher must be compiled and match its agent type"
        );
    }

    #[test]
    fn reject_invalid_regex() {
        let json = r#"{
            "hooks": {
                "PreToolUse": [
                    { "matcher": "[invalid", "hooks": [{ "type": "command", "command": "c.sh" }] }
                ]
            }
        }"#;
        let (specs, errors) = parse_hook_file(json, Path::new("/tmp/test.json"));
        assert!(specs.is_empty());
        assert_eq!(errors.len(), 1);
        assert!(matches!(&errors[0], HookError::InvalidMatcher { .. }));
    }

    #[test]
    fn reject_invalid_json() {
        let json = "this is not valid json {{{";
        let (specs, errors) = parse_hook_file(json, Path::new("/tmp/test.json"));
        assert!(specs.is_empty());
        assert_eq!(errors.len(), 1);
        assert!(matches!(&errors[0], HookError::ParseFile { .. }));
    }

    #[test]
    fn reject_unsupported_handler_type() {
        let json = r#"{
            "hooks": {
                "PreToolUse": [
                    { "hooks": [{ "type": "prompt", "command": "test" }] }
                ]
            }
        }"#;
        let (specs, errors) = parse_hook_file(json, Path::new("/tmp/test.json"));
        assert!(specs.is_empty());
        assert_eq!(errors.len(), 1);
        assert!(matches!(
            &errors[0],
            HookError::UnsupportedHandlerType { .. }
        ));
    }

    #[test]
    fn parse_http_handler_type() {
        let json = r#"{
            "hooks": {
                "PreToolUse": [
                    { "hooks": [{ "type": "http", "url": "https://hooks.example.com/check" }] }
                ]
            }
        }"#;
        let (specs, errors) = parse_hook_file(json, Path::new("/tmp/test.json"));
        assert!(errors.is_empty());
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].handler_type, HandlerType::Http);
        assert!(specs[0].command.is_none());
        assert_eq!(
            specs[0].url.as_deref(),
            Some("https://hooks.example.com/check")
        );
    }

    #[test]
    fn reject_http_handler_without_url() {
        let json = r#"{
            "hooks": {
                "PreToolUse": [
                    { "hooks": [{ "type": "http" }] }
                ]
            }
        }"#;
        let (specs, errors) = parse_hook_file(json, Path::new("/tmp/test.json"));
        assert!(specs.is_empty());
        assert_eq!(errors.len(), 1);
        assert!(matches!(&errors[0], HookError::InvalidConfig { .. }));
    }

    #[test]
    fn source_dir_from_file_path() {
        let json =
            r#"{"hooks":{"SessionStart":[{"hooks":[{"type":"command","command":"x.sh"}]}]}}"#;
        let (specs, _) =
            parse_hook_file(json, Path::new("/home/user/.echo-agent/hooks/safety.json"));
        assert_eq!(
            specs[0].source_dir,
            PathBuf::from("/home/user/.echo-agent/hooks")
        );
    }

    #[test]
    fn empty_hooks_object() {
        let json = r#"{"hooks": {}}"#;
        let (specs, errors) = parse_hook_file(json, Path::new("/tmp/test.json"));
        assert!(errors.is_empty());
        assert!(specs.is_empty());
    }

    #[test]
    fn no_hooks_key() {
        let json = r#"{"theme": "dark"}"#;
        let (specs, errors) = parse_hook_file(json, Path::new("/tmp/test.json"));
        assert!(errors.is_empty());
        assert!(specs.is_empty());
    }

    #[test]
    fn realistic_claude_settings_file() {
        let json = r#"{
            "$schema": "https://json.schemastore.org/claude-code-settings.json",
            "permissions": {
                "allow": ["Bash(npm run build)", "Read(**/src/**)", "Edit(**/src/**)"],
                "deny": ["Bash(rm -rf *)"]
            },
            "model": "claude-sonnet-4-20250514",
            "apiKey": "sk-ant-REDACTED",
            "theme": "dark",
            "customInstructions": "Always use TypeScript",
            "mcpServers": {
                "memory": {
                    "command": "npx",
                    "args": ["-y", "@anthropic/mcp-memory"]
                }
            },
            "hooks": {
                "PreToolUse": [
                    {
                        "matcher": "Bash",
                        "hooks": [
                            {
                                "type": "command",
                                "command": ".claude/hooks/block-dangerous.sh",
                                "timeout": 10
                            }
                        ]
                    }
                ],
                "PostToolUse": [
                    {
                        "matcher": "Write|Edit",
                        "hooks": [
                            { "type": "command", "command": "bun run format || true" }
                        ]
                    }
                ]
            },
            "autoUpdates": true,
            "telemetry": { "enabled": false, "shareUsageData": false }
        }"#;
        let (specs, errors) = parse_hook_file(json, Path::new("/home/user/.claude/settings.json"));
        assert!(errors.is_empty(), "errors: {errors:?}");
        assert_eq!(specs.len(), 2);
        let has_pre = specs.iter().any(|s| s.event == HookEventName::PreToolUse);
        let has_post = specs.iter().any(|s| s.event == HookEventName::PostToolUse);
        assert!(has_pre, "expected PreToolUse hook");
        assert!(has_post, "expected PostToolUse hook");
    }

    #[test]
    fn claude_settings_with_unknown_hook_events_skipped_leniently() {
        let json = r#"{
            "hooks": {
                "PreToolUse": [
                    { "hooks": [{ "type": "command", "command": "check.sh" }] }
                ],
                "PermissionRequest": [
                    { "matcher": "Bash", "hooks": [{ "type": "command", "command": "perm.sh" }] }
                ],
                "TaskCreated": [
                    { "hooks": [{ "type": "command", "command": "task.sh" }] }
                ],
                "FileChanged": [
                    { "matcher": ".envrc", "hooks": [{ "type": "command", "command": "env.sh" }] }
                ],
                "WorktreeCreate": [
                    { "hooks": [{ "type": "command", "command": "wt.sh" }] }
                ],
                "PostToolUse": [
                    { "hooks": [{ "type": "command", "command": "post.sh" }] }
                ]
            }
        }"#;
        let (specs, errors) = parse_hook_file(json, Path::new("/tmp/settings.json"));
        assert!(errors.is_empty(), "unexpected errors: {errors:?}");
        assert_eq!(specs.len(), 2);
        let has_pre = specs.iter().any(|s| s.event == HookEventName::PreToolUse);
        let has_post = specs.iter().any(|s| s.event == HookEventName::PostToolUse);
        assert!(has_pre, "expected PreToolUse hook");
        assert!(has_post, "expected PostToolUse hook");
    }

    /// A `command` referencing a process-env var must be expanded at load time,
    /// removing the dependence on the runtime `sh -c` heuristic for direct-exec
    /// paths with no other shell metachars.
    #[test]
    fn parse_hook_file_expands_env_var_in_command_from_process_env() {
        let key = "ECHO_AGENT_HOOKS_PARSE_TEST_CMD_PROC_ENV";
        with_env_var(key, Some("/usr/local"), || {
            let json = format!(
                r#"{{
                    "hooks": {{
                        "PreToolUse": [
                            {{ "hooks": [{{ "type": "command", "command": "${{{key}}}/check.sh" }}] }}
                        ]
                    }}
                }}"#
            );
            let (specs, errors) = parse_hook_file(&json, Path::new("/tmp/test.json"));
            assert!(errors.is_empty(), "unexpected errors: {errors:?}");
            assert_eq!(specs.len(), 1);
            assert_eq!(specs[0].command, Some(PathBuf::from("/usr/local/check.sh")));
            assert_eq!(
                specs[0].command_raw.as_deref(),
                Some(format!("${{{key}}}/check.sh").as_str())
            );
        });
    }

    /// An HTTP `url` referencing a process-env var must be substituted at load
    /// time so SSRF validation sees the resolved host.
    #[test]
    fn parse_hook_file_expands_env_var_in_url_from_process_env() {
        let key = "ECHO_AGENT_HOOKS_PARSE_TEST_URL_PROC_ENV";
        with_env_var(key, Some("hooks.example.com"), || {
            let json = format!(
                r#"{{
                    "hooks": {{
                        "PreToolUse": [
                            {{ "hooks": [{{ "type": "http", "url": "https://${{{key}}}/check" }}] }}
                        ]
                    }}
                }}"#
            );
            let (specs, errors) = parse_hook_file(&json, Path::new("/tmp/test.json"));
            assert!(errors.is_empty(), "unexpected errors: {errors:?}");
            assert_eq!(specs.len(), 1);
            assert_eq!(
                specs[0].url.as_deref(),
                Some("https://hooks.example.com/check")
            );
            assert_eq!(
                specs[0].url_raw.as_deref(),
                Some(format!("https://${{{key}}}/check").as_str())
            );
        });
    }

    /// A declared `env` map is injected into the process via
    /// `HookSpec::extra_env`.
    #[test]
    fn parse_hook_file_env_map_populates_extra_env() {
        let json = r#"{
            "hooks": {
                "PreToolUse": [
                    {
                        "hooks": [
                            {
                                "type": "command",
                                "command": "echo hi",
                                "env": { "FOO": "bar", "BAZ": "qux" }
                            }
                        ]
                    }
                ]
            }
        }"#;
        let (specs, errors) = parse_hook_file(json, Path::new("/tmp/test.json"));
        assert!(errors.is_empty(), "unexpected errors: {errors:?}");
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].extra_env.len(), 2);
        assert_eq!(
            specs[0].extra_env.get("FOO").map(String::as_str),
            Some("bar")
        );
        assert_eq!(
            specs[0].extra_env.get("BAZ").map(String::as_str),
            Some("qux")
        );
    }

    /// An `env` map value for a var referenced in `command` must win over the
    /// process env when expanding at load time.
    #[test]
    fn parse_hook_file_env_map_feeds_command_expansion() {
        let json = r#"{
            "hooks": {
                "PreToolUse": [
                    {
                        "hooks": [
                            {
                                "type": "command",
                                "command": "${MY_HOOK_ROOT}/check.sh",
                                "env": { "MY_HOOK_ROOT": "/from/env-map" }
                            }
                        ]
                    }
                ]
            }
        }"#;
        let (specs, errors) = parse_hook_file(json, Path::new("/tmp/test.json"));
        assert!(errors.is_empty(), "unexpected errors: {errors:?}");
        assert_eq!(specs.len(), 1);
        assert_eq!(
            specs[0].command,
            Some(PathBuf::from("/from/env-map/check.sh"))
        );
        assert_eq!(specs[0].extra_env.len(), 1);
        assert_eq!(
            specs[0].extra_env.get("MY_HOOK_ROOT").map(String::as_str),
            Some("/from/env-map")
        );
    }

    /// A `command` referencing a var unset at load time must preserve the
    /// literal `${VAR}`, so the runner's pre-flight check stays the single
    /// source of truth for run-time resolvability.
    #[test]
    fn parse_hook_file_preserves_unresolved_env_refs_in_command() {
        let key = "ECHO_AGENT_HOOKS_PARSE_TEST_NEVER_SET_AT_LOAD_TIME";
        with_env_var(key, None, || {
            let json = format!(
                r#"{{
                    "hooks": {{
                        "PreToolUse": [
                            {{ "hooks": [{{ "type": "command", "command": "${{{key}}}/x.sh" }}] }}
                        ]
                    }}
                }}"#
            );
            let (specs, errors) = parse_hook_file(&json, Path::new("/tmp/test.json"));
            assert!(errors.is_empty(), "unexpected errors: {errors:?}");
            assert_eq!(specs.len(), 1);
            let cmd = specs[0]
                .command
                .as_ref()
                .unwrap()
                .to_string_lossy()
                .into_owned();
            assert_eq!(cmd, format!("${{{key}}}/x.sh"));
        });
    }

    /// Symmetry: load-time expansion of `url` must also preserve unset
    /// refs, otherwise a deferred plugin var would be silently stripped.
    #[test]
    fn parse_hook_file_preserves_unresolved_env_refs_in_url() {
        let key = "ECHO_AGENT_HOOKS_PARSE_TEST_URL_NEVER_SET_AT_LOAD_TIME";
        with_env_var(key, None, || {
            let json = format!(
                r#"{{
                    "hooks": {{
                        "PreToolUse": [
                            {{ "hooks": [{{ "type": "http", "url": "https://${{{key}}}/check" }}] }}
                        ]
                    }}
                }}"#
            );
            let (specs, errors) = parse_hook_file(&json, Path::new("/tmp/test.json"));
            assert!(errors.is_empty(), "unexpected errors: {errors:?}");
            assert_eq!(specs.len(), 1);
            let url = specs[0].url.as_deref().unwrap_or("");
            assert_eq!(url, format!("https://${{{key}}}/check"));
        });
    }

    /// Explicit `"env": null` is tolerated and yields an empty `extra_env` map,
    /// rather than serde's default failure mode.
    #[test]
    fn parse_hook_file_env_null_treated_as_empty() {
        let json = r#"{
            "hooks": {
                "PreToolUse": [
                    {
                        "hooks": [
                            { "type": "command", "command": "echo hi", "env": null }
                        ]
                    }
                ]
            }
        }"#;
        let (specs, errors) = parse_hook_file(json, Path::new("/tmp/test.json"));
        assert!(errors.is_empty(), "unexpected errors: {errors:?}");
        assert_eq!(specs.len(), 1);
        assert!(specs[0].extra_env.is_empty());
    }

    /// Env values are stored verbatim: references inside them (e.g. `"${HOME}/x"`)
    /// are NOT recursively expanded. The env map is plumbing, not a template layer.
    #[test]
    fn parse_hook_file_env_values_are_stored_verbatim() {
        let json = r#"{
            "hooks": {
                "PreToolUse": [
                    {
                        "hooks": [
                            {
                                "type": "command",
                                "command": "echo hi",
                                "env": { "BAR": "${HOME}/x" }
                            }
                        ]
                    }
                ]
            }
        }"#;
        let (specs, errors) = parse_hook_file(json, Path::new("/tmp/test.json"));
        assert!(errors.is_empty(), "unexpected errors: {errors:?}");
        assert_eq!(specs.len(), 1);
        assert_eq!(
            specs[0].extra_env.get("BAR").map(String::as_str),
            Some("${HOME}/x"),
            "env values must be stored verbatim, not recursively expanded"
        );
    }

    #[test]
    fn parse_hook_file_matcher_is_not_env_expanded() {
        let key = "ECHO_AGENT_HOOKS_PARSE_TEST_MATCHER_VAR";
        with_env_var(key, Some("expanded_value_should_not_appear"), || {
            let pattern = format!("foo{key}");
            let json = serde_json::json!({
                "hooks": {
                    "PreToolUse": [
                        {
                            "matcher": pattern,
                            "hooks": [
                                { "type": "command", "command": "echo hi" }
                            ]
                        }
                    ]
                }
            });
            let (specs, errors) = parse_hook_file(&json.to_string(), Path::new("/tmp/test.json"));
            assert!(errors.is_empty(), "unexpected errors: {errors:?}");
            assert_eq!(specs.len(), 1);
            assert_eq!(
                specs[0].configured_matcher.as_deref(),
                Some(pattern.as_str())
            );
            let stored = specs[0].configured_matcher.as_deref().unwrap_or("");
            assert!(
                !stored.contains("expanded_value_should_not_appear"),
                "matcher must NOT be env-expanded, got {stored:?}"
            );
        });
    }

    /// A non-string `env` value (e.g. `"PORT": 8080`) fails deserialization; the
    /// whole file surfaces a `ParseFile` error rather than silently dropping it.
    /// Users who need numeric values must quote them (`"PORT": "8080"`).
    #[test]
    fn parse_hook_file_env_value_must_be_string() {
        let json = r#"{
            "hooks": {
                "PreToolUse": [
                    {
                        "hooks": [
                            {
                                "type": "command",
                                "command": "echo hi",
                                "env": { "PORT": 8080 }
                            }
                        ]
                    }
                ]
            }
        }"#;
        let (specs, errors) = parse_hook_file(json, Path::new("/tmp/test.json"));
        assert!(
            specs.is_empty(),
            "expected non-string env value to fail parsing"
        );
        assert!(
            !errors.is_empty(),
            "expected an error for non-string env value, got none"
        );
        assert!(
            errors
                .iter()
                .any(|e| matches!(e, HookError::ParseFile { .. })),
            "expected at least one HookError::ParseFile, got {errors:?}"
        );
    }

    /// User attempts to set runner-reserved keys via the `env` map are stripped
    /// at load time, giving a clear "ignored" signal on top of spawn-time override.
    #[test]
    fn parse_hook_file_strips_runner_reserved_env_keys() {
        let json = r#"{
            "hooks": {
                "PreToolUse": [
                    {
                        "hooks": [
                            {
                                "type": "command",
                                "command": "echo hi",
                                "env": {
                                    "ECHO_AGENT_HOOK_EVENT": "spoofed",
                                    "ECHO_AGENT_HOOK_NAME": "spoofed",
                                    "ECHO_AGENT_SESSION_ID": "spoofed",
                                    "ECHO_AGENT_WORKSPACE_ROOT": "/etc",
                                    "CLAUDE_PROJECT_DIR": "/etc",
                                    "USER_KEY": "kept"
                                }
                            }
                        ]
                    }
                ]
            }
        }"#;
        let (specs, errors) = parse_hook_file(json, Path::new("/tmp/test.json"));
        assert!(errors.is_empty(), "unexpected errors: {errors:?}");
        assert_eq!(specs.len(), 1);
        for reserved in [
            "ECHO_AGENT_HOOK_EVENT",
            "ECHO_AGENT_HOOK_NAME",
            "ECHO_AGENT_SESSION_ID",
            "ECHO_AGENT_WORKSPACE_ROOT",
            "CLAUDE_PROJECT_DIR",
        ] {
            assert!(
                !specs[0].extra_env.contains_key(reserved),
                "reserved key {reserved} must be stripped, got {:?}",
                specs[0].extra_env
            );
        }
        assert_eq!(
            specs[0].extra_env.get("USER_KEY").map(String::as_str),
            Some("kept")
        );
        assert_eq!(specs[0].extra_env.len(), 1);
    }
}
