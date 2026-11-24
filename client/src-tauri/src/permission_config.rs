//! Default-permission rules — reads/writes EchoAgent's `[permission]` config block.
//!
//! EchoAgent evaluates tool-call permission against rules in `~/.echo-agent/config.toml`:
//!
//! ```toml
//! [permission]
//! deny = ["Bash(rm -rf *)"]
//! allow = ["Bash(git *)", "Bash(gh *)"]
//! # OR structured form:
//! rules = [
//!   { action = "allow", tool = "bash", pattern = "git *" },
//!   { action = "deny",  tool = "bash", pattern = "rm -rf *" },
//! ]
//! ```
//!
//! The editor preserves the Runtime's active representation and original rules.
//! Changes require an EchoAgent restart to take effect.

use echo_agent_workspace::permission::{
    rules::parse_permission_rule,
    types::{
        PatternMode, PermissionConfig as RuntimePermissionConfig,
        PermissionRule as RuntimePermissionRule, RuleAction, ToolFilter,
    },
};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::hash::{Hash, Hasher};
use std::sync::{Mutex, OnceLock};
use tauri::{AppHandle, Emitter, State};
use toml::map::Map;
use toml::Value;

use crate::bridge::Permissions;
use crate::commands::AppState;

const RULE_ACTIONS: [(&str, RuleAction); 3] = [
    ("deny", RuleAction::Deny),
    ("allow", RuleAction::Allow),
    ("ask", RuleAction::Ask),
];
const MIXED_RULES: &str =
    "权限配置同时使用简写数组和结构化规则。请在配置文件中保留一种格式后重试。";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionRule {
    action: RuleAction,
    tool: ToolFilter,
    #[serde(default)]
    pattern: Option<String>,
    #[serde(default, skip_serializing_if = "is_glob_mode")]
    pattern_mode: PatternMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    unparsed: Option<UnparsedRule>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
struct UnparsedRule {
    source: String,
    repairable: bool,
}

fn is_glob_mode(mode: &PatternMode) -> bool {
    *mode == PatternMode::Glob
}

impl Hash for PermissionRule {
    fn hash<H: Hasher>(&self, state: &mut H) {
        std::mem::discriminant(&self.action).hash(state);
        std::mem::discriminant(&self.tool).hash(state);
        self.pattern.hash(state);
        std::mem::discriminant(&self.pattern_mode).hash(state);
        self.unparsed.hash(state);
    }
}

impl From<RuntimePermissionRule> for PermissionRule {
    fn from(rule: RuntimePermissionRule) -> Self {
        Self {
            action: rule.action,
            tool: rule.tool,
            pattern: rule.pattern,
            pattern_mode: rule.pattern_mode,
            unparsed: None,
        }
    }
}

struct StoredRules {
    compact: bool,
    entries: Vec<(PermissionRule, Value)>,
}

// These were the exact prefixes emitted by the old desktop settings editor.
// A bare legacy alias could previously parse as a literal Any pattern, so
// recognize aliases before letting the Runtime parser interpret the string.
fn legacy_tool_rule(raw: &str) -> Option<(ToolFilter, Option<String>)> {
    let raw = raw.trim();
    let (name, pattern) = match raw.split_once('(') {
        Some((name, pattern)) => (name, Some(pattern.strip_suffix(')')?.trim())),
        None => (raw, None),
    };
    let tool = match name {
        "Mcp" => ToolFilter::Mcp,
        "Webfetch" => ToolFilter::WebFetch,
        "Any" => ToolFilter::Any,
        _ => return None,
    };
    Some((
        tool,
        pattern.filter(|value| !value.is_empty()).map(str::to_owned),
    ))
}

fn read_compact_entry(value: &Value, action: RuleAction) -> PermissionRule {
    let legacy = value.as_str().and_then(legacy_tool_rule);
    let is_legacy = legacy.is_some();
    if !is_legacy {
        if let Some(rule) = value
            .as_str()
            .and_then(|raw| parse_permission_rule(raw, action).ok())
        {
            return rule.into();
        }
    }
    let (tool, pattern) = legacy.unwrap_or((ToolFilter::Any, None));
    let mut rule = PermissionRule {
        action,
        tool,
        pattern,
        pattern_mode: PatternMode::Glob,
        unparsed: None,
    };
    let repairable = match is_legacy.then(|| encode_rule(&rule, true)) {
        Some(Ok((_, parsed))) => {
            rule = parsed.into();
            true
        }
        _ => false,
    };
    rule.unparsed = Some(UnparsedRule {
        source: value
            .as_str()
            .map_or_else(|| value.to_string(), str::to_owned),
        repairable,
    });
    rule
}

fn stored_rules(config: &Value) -> Result<StoredRules, String> {
    let Some(permission) = config.get("permission") else {
        return Ok(StoredRules {
            compact: true,
            entries: Vec::new(),
        });
    };
    let table = permission
        .as_table()
        .ok_or("权限配置必须是 TOML 表，请修复后重试。")?;
    let compact = RULE_ACTIONS.iter().any(|(key, _)| table.contains_key(*key));
    if let Some(structured) = table.get("rules") {
        if compact {
            return Err(MIXED_RULES.into());
        }
        let parsed: RuntimePermissionConfig = permission
            .clone()
            .try_into()
            .map_err(|_| "无法读取结构化权限规则，请检查配置文件。")?;
        let originals = structured
            .as_array()
            .ok_or("结构化权限规则必须是数组，请检查配置文件。")?;
        return Ok(StoredRules {
            compact: false,
            entries: parsed
                .rules
                .into_iter()
                .map(PermissionRule::from)
                .zip(originals.iter().cloned())
                .collect(),
        });
    }
    let mut entries = Vec::new();
    for (key, action) in RULE_ACTIONS {
        let Some(value) = table.get(key) else {
            continue;
        };
        let rules = value
            .as_array()
            .ok_or_else(|| format!("权限规则 {key} 必须是数组，请检查配置文件。"))?;
        for value in rules {
            entries.push((read_compact_entry(value, action), value.clone()));
        }
    }
    Ok(StoredRules {
        compact: true,
        entries,
    })
}

fn encode_rule(
    rule: &PermissionRule,
    compact: bool,
) -> Result<(String, RuntimePermissionRule), String> {
    if rule.unparsed.is_some() {
        return Err("待修复规则已在配置文件中发生变化，请重新打开权限设置。".into());
    }
    let pattern = rule.pattern.as_deref().unwrap_or_default().trim();
    if pattern.contains(['\n', '\r']) {
        return Err("权限匹配范围必须是单行文字。".into());
    }
    let prefix = match rule.tool {
        ToolFilter::Bash => "Bash",
        ToolFilter::Read => "Read",
        ToolFilter::Edit => "Edit",
        ToolFilter::Grep => "Grep",
        ToolFilter::Mcp => "MCPTool",
        ToolFilter::WebFetch => "WebFetch",
        ToolFilter::WebSearch => "WebSearch",
        ToolFilter::Any => "",
    };
    if rule.pattern_mode == PatternMode::Domain
        && (rule.tool != ToolFilter::WebFetch || pattern.is_empty())
    {
        return Err("域名匹配需要访问网页规则和非空域名。".into());
    }
    let raw = if prefix.is_empty() || pattern.is_empty() {
        if prefix.is_empty() {
            pattern.into()
        } else {
            prefix.into()
        }
    } else {
        let encoded = pattern
            .replace('\\', "\\\\")
            .replace('(', "\\(")
            .replace(')', "\\)");
        let domain = if rule.pattern_mode == PatternMode::Domain {
            "domain:"
        } else {
            ""
        };
        format!("{prefix}({domain}{encoded})")
    };
    let parsed = parse_permission_rule(&raw, rule.action)
        .map_err(|_| "无法解析权限规则，请检查匹配范围。")?;
    if parsed.tool != rule.tool
        || (compact && legacy_tool_rule(&raw).is_some())
        || (parsed.pattern_mode == PatternMode::Domain && rule.tool != ToolFilter::WebFetch)
        || (!pattern.is_empty() && pattern != "*" && parsed.pattern.is_none())
    {
        return Err("此匹配范围无法按所选工具类型保存，请调整权限规则。".into());
    }
    Ok((raw, parsed))
}

fn new_rule_value(rule: &PermissionRule, compact: bool) -> Result<Value, String> {
    let (raw, parsed) = encode_rule(rule, compact)?;
    if compact {
        Ok(Value::String(raw))
    } else {
        Value::try_from(parsed).map_err(|_| "无法保存结构化权限规则。".into())
    }
}

fn apply_rules(config: &mut Value, rules: Vec<PermissionRule>) -> Result<(), String> {
    let StoredRules { compact, entries } = stored_rules(config)?;
    if entries.iter().map(|(rule, _)| rule).eq(rules.iter()) {
        return Ok(());
    }

    let mut originals = HashMap::<_, VecDeque<_>>::new();
    for (rule, raw) in entries {
        originals.entry(rule).or_default().push_back(raw);
    }
    let mut saved = Vec::with_capacity(rules.len());
    for rule in rules {
        let raw = match originals.get_mut(&rule).and_then(VecDeque::pop_front) {
            Some(raw) => raw,
            None => new_rule_value(&rule, compact)?,
        };
        saved.push((rule.action, raw));
    }

    let table = config
        .as_table_mut()
        .ok_or("配置根节点必须是 TOML 表。")?
        .entry("permission")
        .or_insert_with(|| Value::Table(Map::new()))
        .as_table_mut()
        .ok_or("权限配置必须是 TOML 表。")?;
    if compact {
        for (key, action) in RULE_ACTIONS {
            let values: Vec<_> = saved
                .iter()
                .filter(|(saved_action, _)| *saved_action == action)
                .map(|(_, value)| value.clone())
                .collect();
            if !values.is_empty() || table.contains_key(key) {
                table.insert(key.into(), Value::Array(values));
            }
        }
    } else {
        table.insert(
            "rules".into(),
            Value::Array(saved.into_iter().map(|(_, value)| value).collect()),
        );
    }
    Ok(())
}

pub fn read_rules() -> Result<Vec<PermissionRule>, String> {
    let config = crate::providers::read_config_checked()?;
    Ok(stored_rules(&config)?
        .entries
        .into_iter()
        .map(|(rule, _)| rule)
        .collect())
}

#[tauri::command]
pub fn permission_list(_state: State<'_, AppState>) -> Result<Vec<PermissionRule>, String> {
    read_rules()
}

/// Save a complete edit while preserving the Runtime representation.
#[tauri::command]
pub fn permission_save(
    _state: State<'_, AppState>,
    rules: Vec<PermissionRule>,
) -> Result<(), String> {
    crate::providers::update_config_checked(|config| apply_rules(config, rules))
}

// ========================================================================
// Agent / assistant defaults — `[models] default` + `[ui] default_selected_permission`
// ========================================================================

/// The current "new session" defaults that affect every agent/assistant.
/// Mirrors EchoAgent's `[models] default` and `[ui] default_selected_permission`
/// config keys (see user-guide/05-configuration.md).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentDefaults {
    /// Model id used for new sessions (`[models] default`). Empty = EchoAgent
    /// falls back to its built-in default (`echo-agent-build`).
    #[serde(default)]
    pub default_model: String,
    /// Default permission selection on the FIRST approval prompt
    /// (`[ui] default_selected_permission`). One of:
    /// "always_allow_all_sessions" | "always_allow_this_session" |
    /// "allow_once" | "always_deny_all_sessions" | "deny_once".
    /// Empty = EchoAgent's built-in default (no preselection).
    #[serde(default)]
    pub default_permission: String,
    /// Whether to show "Always allow" options on permission prompts
    /// (`[ui] remember_tool_approvals`). Null = unset.
    #[serde(default)]
    pub remember_tool_approvals: Option<bool>,
}

/// Read the agent defaults from config.toml.
pub fn read_defaults() -> AgentDefaults {
    let config = crate::providers::read_config();
    let mut out = AgentDefaults::default();
    if let Some(models) = config.get("models").and_then(Value::as_table) {
        if let Some(d) = models.get("default").and_then(Value::as_str) {
            out.default_model = d.to_string();
        }
    }
    if let Some(ui) = config.get("ui").and_then(Value::as_table) {
        if let Some(p) = ui
            .get("default_selected_permission")
            .and_then(Value::as_str)
        {
            out.default_permission = p.to_string();
        }
        if let Some(r) = ui.get("remember_tool_approvals").and_then(Value::as_bool) {
            out.remember_tool_approvals = Some(r);
        }
    }
    out
}

/// Write the agent defaults to config.toml. We preserve all other keys in
/// `[models]` and `[ui]` (merge, not replace).
pub fn write_defaults(defaults: &AgentDefaults) -> Result<(), String> {
    crate::providers::update_config(|config| {
        let root = config.as_table_mut().ok_or("config root is not a table")?;

        // [models].default
        if !root.contains_key("models") {
            root.insert("models".into(), Value::Table(Map::new()));
        }
        if let Some(models) = root.get_mut("models").and_then(Value::as_table_mut) {
            if defaults.default_model.is_empty() {
                models.remove("default");
            } else {
                models.insert(
                    "default".into(),
                    Value::String(defaults.default_model.clone()),
                );
            }
        }

        // [ui].default_selected_permission + remember_tool_approvals
        if !root.contains_key("ui") {
            root.insert("ui".into(), Value::Table(Map::new()));
        }
        if let Some(ui) = root.get_mut("ui").and_then(Value::as_table_mut) {
            if defaults.default_permission.is_empty() {
                ui.remove("default_selected_permission");
            } else {
                ui.insert(
                    "default_selected_permission".into(),
                    Value::String(defaults.default_permission.clone()),
                );
            }
            match defaults.remember_tool_approvals {
                Some(b) => {
                    ui.insert("remember_tool_approvals".into(), Value::Boolean(b));
                }
                None => {
                    ui.remove("remember_tool_approvals");
                }
            }
        }

        Ok(())
    })
}

/// Read the agent defaults (new-session model + default permission).
#[tauri::command]
pub fn agents_defaults_get(_state: State<'_, AppState>) -> AgentDefaults {
    read_defaults()
}

/// Save the agent defaults. Atomic write to config.toml.
#[tauri::command]
pub fn agents_defaults_save(
    _state: State<'_, AppState>,
    defaults: AgentDefaults,
) -> Result<(), String> {
    write_defaults(&defaults)
}

// ========================================================================
// Task-scoped permission mode ("ask" | "auto" | "always-approve")
// ========================================================================

/// Canonical permission modes EchoAgent accepts (see echo-agent-build
/// `util/config/permissions.rs::parse_permission_mode_canonical`).
pub const PERMISSION_MODES: [&str; 3] = ["ask", "auto", "always-approve"];
const AUTO_MODE_UNAVAILABLE_REASON: &str = "自动模式已被本机配置、环境设置或组织策略关闭";
const ALWAYS_APPROVE_UNAVAILABLE_REASON: &str = "始终允许已被本机要求或组织策略禁用";

/// Runtime flags corresponding to the canonical desktop permission mode.
/// Keeping this mapping in one place prevents launch defaults and per-session
/// metadata from drifting apart.
pub(crate) fn permission_mode_flags(mode: &str) -> (bool, bool) {
    (mode == "always-approve", mode == "auto")
}

fn auto_mode_available() -> bool {
    echo_agent_runtime::util::config::auto_permission_mode_enabled_from_disk()
}

fn always_approve_policy_block() -> Option<String> {
    let effective = echo_agent_runtime::util::config::effective_yolo_for_launch(
        false,
        Some("always-approve"),
        None,
    );
    (!effective.yolo).then(|| ALWAYS_APPROVE_UNAVAILABLE_REASON.to_string())
}

pub(crate) fn ensure_always_approve_available() -> Result<(), String> {
    match always_approve_policy_block() {
        Some(reason) => Err(reason),
        None => Ok(()),
    }
}

pub(crate) fn effective_permission_mode(
    configured_mode: &str,
    auto_available: bool,
    always_approve_available: bool,
) -> String {
    match configured_mode {
        "auto" if !auto_available => "ask".into(),
        "always-approve" if !always_approve_available => "ask".into(),
        _ => configured_mode.into(),
    }
}

fn validate_permission_mode_selection(
    mode: &str,
    auto_available: bool,
    always_approve_available: bool,
) -> Result<(), String> {
    if !PERMISSION_MODES.contains(&mode) {
        return Err(format!("unknown permission mode: {mode}"));
    }
    if mode == "auto" && !auto_available {
        return Err(AUTO_MODE_UNAVAILABLE_REASON.into());
    }
    if mode == "always-approve" && !always_approve_available {
        return Err(always_approve_policy_block()
            .unwrap_or_else(|| ALWAYS_APPROVE_UNAVAILABLE_REASON.into()));
    }
    Ok(())
}

/// Runtime default for sessions that do not supply explicit metadata. User
/// choices are never read from global config: every interactive task owns its
/// mode, while organization policy remains the only global override.
pub fn read_permission_mode() -> String {
    let configured = crate::policy::locked_permission_mode().unwrap_or_else(|| "ask".into());
    effective_permission_mode(
        &configured,
        auto_mode_available(),
        always_approve_policy_block().is_none(),
    )
}

/// Safe fallback used by non-interactive automation cleanup. Interactive
/// create/load paths always pass a task-owned mode explicitly.
pub(crate) fn permission_mode_for_session() -> String {
    read_permission_mode()
}

/// Remove obsolete global user preference keys. Keeping them on disk would let
/// another Runtime entry point silently reintroduce cross-task authorization.
fn remove_legacy_global_permission_mode(config: &mut Value) -> Result<bool, String> {
    let root = config.as_table_mut().ok_or("config root is not a table")?;
    let Some(ui) = root.get_mut("ui").and_then(Value::as_table_mut) else {
        return Ok(false);
    };
    let mut changed = false;
    for key in ["permission_mode", "approval_mode", "yolo"] {
        changed |= ui.remove(key).is_some();
    }
    Ok(changed)
}

pub fn clear_global_permission_mode() -> Result<(), String> {
    let config = crate::providers::read_config();
    let has_legacy_mode = config
        .get("ui")
        .and_then(Value::as_table)
        .is_some_and(|ui| {
            ui.contains_key("permission_mode")
                || ui.contains_key("approval_mode")
                || ui.contains_key("yolo")
        });
    if !has_legacy_mode {
        return Ok(());
    }
    crate::providers::update_config(|config| {
        remove_legacy_global_permission_mode(config)?;
        Ok(())
    })
}

/// Resolve the explicit mode selected for a newly-created interactive task.
/// Managed policy is authoritative; otherwise every new task starts from the
/// caller's draft selection (which the desktop defaults to Ask).
pub(crate) fn resolve_new_session_permission_mode(
    requested: Option<&str>,
) -> Result<String, String> {
    if let Some(locked) = crate::policy::locked_permission_mode() {
        validate_permission_mode_selection(
            &locked,
            auto_mode_available(),
            always_approve_policy_block().is_none(),
        )?;
        return Ok(locked);
    }
    let requested = requested.unwrap_or("ask");
    validate_permission_mode_selection(
        requested,
        auto_mode_available(),
        always_approve_policy_block().is_none(),
    )?;
    Ok(requested.to_string())
}

/// A preview is a comparison, not authority to override managed policy.
pub(crate) fn require_new_session_permission_preview(
    resolved: Result<String, String>,
    expected: Option<&str>,
) -> Result<String, String> {
    let Some(expected) = expected else {
        return resolved;
    };
    let actual = resolved.map_err(|error| format!("任务默认权限已变化，无法创建任务：{error}"))?;
    if actual != expected {
        return Err("任务默认权限已变化，请确认当前权限后重新发送".into());
    }
    Ok(actual)
}

fn configured_session_permission_mode(session_id: Option<&str>) -> String {
    match session_id {
        Some(session_id) => crate::groups::configured_permission_mode_for_session(session_id)
            .unwrap_or_else(|| crate::meta::permission_mode(session_id)),
        None => "ask".into(),
    }
}

pub(crate) fn effective_session_permission_mode(session_id: &str) -> String {
    let configured = crate::policy::locked_permission_mode()
        .unwrap_or_else(|| configured_session_permission_mode(Some(session_id)));
    effective_permission_mode(
        &configured,
        auto_mode_available(),
        always_approve_policy_block().is_none(),
    )
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionModeStatus {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Effective mode the Runtime can honor now.
    pub permission_mode: String,
    /// Configured or policy-selected mode before the Auto capability clamp.
    pub configured_permission_mode: String,
    pub auto_mode_available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_mode_unavailable_reason: Option<String>,
    pub always_approve_available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub always_approve_unavailable_reason: Option<String>,
    pub locked: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub locked_reason: Option<String>,
    pub runtime_sync_state: PermissionRuntimeSyncState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime_applied_mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime_sync_error: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum PermissionRuntimeSyncState {
    #[default]
    Offline,
    Syncing,
    Synced,
    Failed,
}

#[derive(Debug, Clone, Default)]
struct RuntimePermissionSync {
    state: PermissionRuntimeSyncState,
    applied_mode: Option<String>,
    error: Option<String>,
}

fn session_permission_sync() -> &'static Mutex<HashMap<String, RuntimePermissionSync>> {
    static STATE: OnceLock<Mutex<HashMap<String, RuntimePermissionSync>>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn runtime_permission_sync() -> &'static Mutex<RuntimePermissionSync> {
    static STATE: OnceLock<Mutex<RuntimePermissionSync>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(RuntimePermissionSync::default()))
}

pub(crate) fn permission_transition_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

pub(crate) fn mark_runtime_permission_mode_starting(mode: &str) {
    session_permission_sync().lock().unwrap().clear();
    *runtime_permission_sync().lock().unwrap() = RuntimePermissionSync {
        state: PermissionRuntimeSyncState::Syncing,
        applied_mode: Some(mode.into()),
        error: None,
    };
}

pub(crate) fn mark_runtime_permission_mode_synced(mode: &str) {
    *runtime_permission_sync().lock().unwrap() = RuntimePermissionSync {
        state: PermissionRuntimeSyncState::Synced,
        applied_mode: Some(mode.into()),
        error: None,
    };
}

pub(crate) fn mark_runtime_permission_mode_offline(error: Option<String>) {
    session_permission_sync().lock().unwrap().clear();
    *runtime_permission_sync().lock().unwrap() = RuntimePermissionSync {
        state: PermissionRuntimeSyncState::Offline,
        applied_mode: None,
        error,
    };
}

pub(crate) fn mark_session_permission_mode_syncing(session_id: &str, applied_mode: &str) {
    session_permission_sync().lock().unwrap().insert(
        session_id.to_string(),
        RuntimePermissionSync {
            state: PermissionRuntimeSyncState::Syncing,
            applied_mode: Some(applied_mode.into()),
            error: None,
        },
    );
}

pub(crate) fn mark_session_permission_mode_synced(session_id: &str, mode: &str) {
    session_permission_sync().lock().unwrap().insert(
        session_id.to_string(),
        RuntimePermissionSync {
            state: PermissionRuntimeSyncState::Synced,
            applied_mode: Some(mode.into()),
            error: None,
        },
    );
}

fn mark_session_permission_mode_failed(session_id: &str, applied_mode: &str, error: String) {
    session_permission_sync().lock().unwrap().insert(
        session_id.to_string(),
        RuntimePermissionSync {
            state: PermissionRuntimeSyncState::Failed,
            applied_mode: Some(applied_mode.into()),
            error: Some(error),
        },
    );
}

pub(crate) fn forget_session_permission_mode(session_id: &str) {
    session_permission_sync().lock().unwrap().remove(session_id);
}

/// Bridge-side automatic approval is session-scoped and only becomes active
/// after that exact Runtime session acknowledged its mode.
pub(crate) fn is_session_permission_mode_active(session_id: &str, mode: &str) -> bool {
    if effective_session_permission_mode(session_id) != mode {
        return false;
    }
    session_permission_sync()
        .lock()
        .unwrap()
        .get(session_id)
        .is_some_and(|runtime| {
            runtime.state == PermissionRuntimeSyncState::Synced
                && runtime.applied_mode.as_deref() == Some(mode)
        })
}

pub(crate) fn runtime_permission_mode_is_current() -> bool {
    let desired_mode = read_permission_mode();
    let runtime = runtime_permission_sync().lock().unwrap();
    runtime.state == PermissionRuntimeSyncState::Synced
        && runtime.applied_mode.as_deref() == Some(desired_mode.as_str())
}

pub(crate) fn permission_mode_status(
    agent_running: bool,
    session_id: Option<&str>,
) -> PermissionModeStatus {
    let policy_mode = crate::policy::locked_permission_mode();
    let configured_permission_mode = policy_mode
        .clone()
        .unwrap_or_else(|| configured_session_permission_mode(session_id));
    let auto_mode_available = auto_mode_available();
    let always_approve_unavailable_reason = always_approve_policy_block();
    let always_approve_available = always_approve_unavailable_reason.is_none();
    let permission_mode = effective_permission_mode(
        &configured_permission_mode,
        auto_mode_available,
        always_approve_available,
    );
    let mut runtime = match session_id {
        Some(session_id) => session_permission_sync()
            .lock()
            .unwrap()
            .get(session_id)
            .cloned()
            .unwrap_or_default(),
        None => runtime_permission_sync().lock().unwrap().clone(),
    };
    if agent_running
        && runtime.state == PermissionRuntimeSyncState::Synced
        && runtime
            .applied_mode
            .as_deref()
            .is_some_and(|applied| applied != permission_mode)
    {
        runtime.state = PermissionRuntimeSyncState::Failed;
        runtime.error = Some(
            "权限配置或组织策略已变化，但运行中的 Agent 尚未应用；请重试同步或重启 Agent".into(),
        );
    }
    PermissionModeStatus {
        session_id: session_id.map(str::to_string),
        permission_mode,
        configured_permission_mode,
        auto_mode_available,
        auto_mode_unavailable_reason: (!auto_mode_available)
            .then(|| AUTO_MODE_UNAVAILABLE_REASON.to_string()),
        always_approve_available,
        always_approve_unavailable_reason,
        locked: policy_mode.is_some(),
        locked_reason: policy_mode.map(|mode| format!("权限模式已被组织策略锁定为 {mode}")),
        runtime_sync_state: if agent_running {
            runtime.state
        } else {
            PermissionRuntimeSyncState::Offline
        },
        runtime_applied_mode: agent_running.then_some(runtime.applied_mode).flatten(),
        runtime_sync_error: runtime.error,
    }
}

fn valid_session_id(value: &str) -> bool {
    !value.trim().is_empty() && value.chars().count() <= 256 && !value.chars().any(char::is_control)
}

/// Effective permission mode and capabilities for one task. `None` describes
/// the safe defaults for an as-yet-uncreated task on the home screen.
#[tauri::command]
pub fn permission_mode_get(
    state: State<'_, AppState>,
    session_id: Option<String>,
) -> Result<PermissionModeStatus, String> {
    if session_id
        .as_deref()
        .is_some_and(|id| !valid_session_id(id))
    {
        return Err("会话 ID 无效或过长".into());
    }
    let agent_running = state.tx.lock().unwrap().is_some();
    Ok(permission_mode_status(agent_running, session_id.as_deref()))
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionModeSetResult {
    #[serde(flatten)]
    pub status: PermissionModeStatus,
    pub agent_running: bool,
    pub runtime_synced: bool,
    pub resolved_pending: usize,
    pub remaining_pending: usize,
    pub resolved_permissions: Vec<crate::bridge::PermissionClosedFrontend>,
}

async fn notify_runtime_permission_mode(
    tx: &echo_agent_acp::AcpAgentTx,
    mode: &str,
    session_ids: Option<&[String]>,
    exclude_session_ids: Option<&[String]>,
    prompt_on_auto_deny: bool,
) -> Result<(), String> {
    let (yolo_mode, auto_mode) = permission_mode_flags(mode);
    let mut params = serde_json::json!({
        "permission_mode": mode,
        "yolo_mode": yolo_mode,
        "auto_mode": auto_mode,
        "promptOnAutoDeny": prompt_on_auto_deny,
        // Scope the generic Runtime API to sessions created by this desktop
        // integration. The shipped product has one desktop client, while the
        // embedded Runtime still requires an explicit origin boundary.
        "clientIdentifier": crate::agent_runtime::DESKTOP_CLIENT_IDENTIFIER,
    });
    if let Some(session_ids) = session_ids {
        params["sessionIds"] = serde_json::json!(session_ids);
    }
    if let Some(exclude_session_ids) = exclude_session_ids {
        params["excludeSessionIds"] = serde_json::json!(exclude_session_ids);
    }
    let params = crate::ext::raw_params(&params);
    let notification =
        agent_client_protocol::ExtNotification::new("echo.agent/yolo_mode_changed", params);
    let response = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        echo_agent_acp::acp_send(notification, tx),
    )
    .await
    .map_err(|_| "运行中的 Agent 权限同步超时".to_string())?;
    response.map_err(|error| format!("运行中的 Agent 权限同步失败：{error:?}"))
}

pub(crate) async fn sync_runtime_session_permission_mode(
    tx: &echo_agent_acp::AcpAgentTx,
    session_id: &str,
    mode: &str,
) -> Result<(), String> {
    notify_runtime_permission_mode(tx, mode, Some(&[session_id.to_string()]), None, false).await
}

pub(crate) async fn sync_group_session_permission_modes(
    tx: &echo_agent_acp::AcpAgentTx,
    session_ids: &[String],
    mode: &str,
) -> Result<(), String> {
    if session_ids.is_empty() {
        return Ok(());
    }
    notify_runtime_permission_mode(tx, mode, Some(session_ids), None, true).await
}

/// Change one task's permission mode. The Runtime update is explicitly scoped
/// to `session_id`; other resident/background tasks and their pending requests
/// are never touched. Already-visible requests remain manual decisions because
/// they were created under the previous mode.
#[tauri::command]
pub async fn permission_mode_set(
    app: AppHandle,
    state: State<'_, AppState>,
    permissions: State<'_, Permissions>,
    session_id: String,
    mode: String,
) -> Result<PermissionModeSetResult, String> {
    if !valid_session_id(&session_id) {
        return Err("会话 ID 无效或过长".into());
    }
    if let Some(group_id) = crate::groups::permission_scope_for_session(&session_id) {
        return crate::groups::set_permission_mode(&app, &state, &permissions, &group_id, &mode)
            .await;
    }
    let _transition_guard = permission_transition_lock().lock().await;
    // Only sessions admitted into this Runtime generation may be mutated.
    // Revalidate after acquiring the transition lock: session load/create and
    // policy changes use the same lock, so this cannot race a Runtime change.
    // A renderer-provided id is not an authorization boundary by itself.
    state.session_workspace(&session_id)?;
    let auto_available = auto_mode_available();
    let always_available = always_approve_policy_block().is_none();
    validate_permission_mode_selection(&mode, auto_available, always_available)?;
    if let Some(locked) = crate::policy::locked_permission_mode() {
        if locked != mode {
            return Err(format!("权限模式已被策略锁定为 {locked}"));
        }
    }

    let tx = state.tx.lock().unwrap().clone();
    let Some(tx) = tx.as_ref() else {
        return Err("Agent 未运行，无法为当前任务切换权限".into());
    };
    let agent_running = true;
    let previous_mode = effective_session_permission_mode(&session_id);
    mark_session_permission_mode_syncing(&session_id, &previous_mode);

    let target_ids = [session_id.clone()];
    let mut sync_error = None;
    let mut runtime_synced = false;
    for attempt in 1..=2 {
        match notify_runtime_permission_mode(tx, &mode, Some(&target_ids), None, false).await {
            Ok(()) => {
                runtime_synced = true;
                break;
            }
            Err(error) => {
                tracing::warn!(%error, attempt, %session_id, permission_mode = %mode, "task permission mode sync was not acknowledged");
                sync_error = Some(error);
            }
        }
    }

    if !runtime_synced {
        let error = sync_error.unwrap_or_else(|| "运行中的 Agent 未确认权限同步".into());
        if notify_runtime_permission_mode(tx, &previous_mode, Some(&target_ids), None, false)
            .await
            .is_ok()
        {
            mark_session_permission_mode_synced(&session_id, &previous_mode);
            return Err(format!("{error}；当前任务已恢复原权限"));
        }
        mark_session_permission_mode_failed(&session_id, &previous_mode, error.clone());
        if state.mark_runtime_dead_if_current(tx, &error) {
            mark_runtime_permission_mode_offline(Some(error.clone()));
            let _ = app.emit("agent://agent-died", serde_json::json!({ "reason": error }));
        }
        return Err("权限同步与回滚均未获确认，Agent 已安全停止".into());
    }

    if crate::policy::locked_permission_mode().is_none() {
        if let Err(error) = crate::meta::set_permission_mode(&session_id, &mode) {
            if notify_runtime_permission_mode(tx, &previous_mode, Some(&target_ids), None, false)
                .await
                .is_ok()
            {
                mark_session_permission_mode_synced(&session_id, &previous_mode);
                return Err(format!("权限模式无法保存：{error}；已恢复原权限"));
            }
            if state.mark_runtime_dead_if_current(tx, &error) {
                mark_runtime_permission_mode_offline(Some(error.clone()));
                let _ = app.emit("agent://agent-died", serde_json::json!({ "reason": error }));
            }
            return Err("权限模式无法保存且回滚失败，Agent 已安全停止".into());
        }
    }
    mark_session_permission_mode_synced(&session_id, &mode);

    let closed = Vec::new();
    let remaining_pending = permissions.list(Some(&session_id)).await.len();
    let result = PermissionModeSetResult {
        status: permission_mode_status(agent_running, Some(&session_id)),
        agent_running,
        runtime_synced,
        resolved_pending: closed.len(),
        remaining_pending,
        resolved_permissions: closed,
    };
    let _ = app.emit("agent://permission-mode", &result);
    Ok(result)
}

// ---------- unit tests ----------

#[cfg(test)]
mod tests {
    use super::*;

    fn rules_from(config: &Value) -> Vec<PermissionRule> {
        stored_rules(config)
            .unwrap()
            .entries
            .into_iter()
            .map(|(rule, _)| rule)
            .collect()
    }

    fn draft_rule(action: RuleAction, tool: ToolFilter, pattern: Option<&str>) -> PermissionRule {
        PermissionRule {
            action,
            tool,
            pattern: pattern.map(str::to_string),
            pattern_mode: PatternMode::Glob,
            unparsed: None,
        }
    }

    #[test]
    fn structured_domain_rule_and_unedited_fields_survive_a_settings_edit() {
        let mut config: Value = toml::from_str(
            r#"
                [permission]
                prompt_policy = "deny"
                [[permission.rules]]
                action = "deny"
                tool = "webfetch"
                pattern = "example.invalid"
                pattern_mode = "domain"
                editor_metadata = "retain"
            "#,
        )
        .unwrap();
        let original = config["permission"].clone();
        let mut rules = rules_from(&config);
        let rendered = serde_json::to_value(&rules[0]).unwrap();
        assert_eq!(rendered["patternMode"], "domain");
        rules[0] = serde_json::from_value(rendered).unwrap();
        rules.push(draft_rule(RuleAction::Ask, ToolFilter::Read, None));
        apply_rules(&mut config, rules).unwrap();

        let permission = &config["permission"];
        let parsed: RuntimePermissionConfig = permission.clone().try_into().unwrap();
        assert_eq!(permission["prompt_policy"], original["prompt_policy"]);
        assert_eq!(permission["rules"][0], original["rules"][0]);
        assert!(permission.get("ask").is_none());
        assert_eq!(parsed.rules.len(), 2);
        assert_eq!(parsed.rules[0].action, RuleAction::Deny);
        assert_eq!(parsed.rules[0].tool, ToolFilter::WebFetch);
        assert_eq!(parsed.rules[0].pattern_mode, PatternMode::Domain);
        assert_eq!(parsed.rules[0].pattern.as_deref(), Some("example.invalid"));
        assert_eq!(parsed.rules[1].tool, ToolFilter::Read);
    }

    #[test]
    fn compact_settings_keep_original_syntax_and_write_runtime_tool_names() {
        let mut config: Value = toml::from_str(
            r#"
                [permission]
                deny = ['WebFetch(domain:example.invalid)', 'mcp__audit', 'Bash(echo \(x\))', 'Bash(echo (y))']
                ask = []
                keep = true
            "#,
        )
        .unwrap();
        let original = config["permission"]["deny"].clone();
        let mut rules = rules_from(&config);
        assert_eq!(rules[0].pattern_mode, PatternMode::Domain);
        assert_eq!(rules[1].tool, ToolFilter::Mcp);
        assert_eq!(rules[1].pattern.as_deref(), Some("audit__*"));
        assert_eq!(rules[2].pattern.as_deref(), Some("echo (x)"));
        assert_eq!(rules[3].pattern.as_deref(), Some("echo (y)"));
        rules.push(draft_rule(
            RuleAction::Deny,
            ToolFilter::Mcp,
            Some("new__*"),
        ));
        rules.push(draft_rule(
            RuleAction::Deny,
            ToolFilter::WebFetch,
            Some("domain:second.invalid"),
        ));
        apply_rules(&mut config, rules).unwrap();

        let table = &config["permission"];
        let compact = table["deny"].as_array().unwrap();
        assert!(compact.starts_with(original.as_array().unwrap()));
        assert_eq!(compact[4].as_str(), Some("MCPTool(new__*)"));
        assert_eq!(compact[5].as_str(), Some("WebFetch(domain:second.invalid)"));
        assert_eq!(table["ask"].as_array().unwrap().len(), 0);
        assert_eq!(table["keep"].as_bool(), Some(true));
        assert_eq!(rules_from(&config)[5].pattern_mode, PatternMode::Domain);
    }

    #[test]
    fn new_compact_rules_preserve_literal_parentheses_and_explicit_tool_scope() {
        let mut config = Value::Table(Map::new());
        apply_rules(
            &mut config,
            vec![
                draft_rule(RuleAction::Deny, ToolFilter::Bash, Some(r"echo (hello)\")),
                draft_rule(RuleAction::Deny, ToolFilter::Any, None),
            ],
        )
        .unwrap();
        let rules = rules_from(&config);
        assert_eq!(rules[0].pattern.as_deref(), Some(r"echo (hello)\"));
        assert_eq!(
            rules[1],
            draft_rule(RuleAction::Deny, ToolFilter::Any, None)
        );
        assert_eq!(config["permission"]["deny"][1].as_str(), Some(""));
        let original = config.clone();
        assert!(apply_rules(
            &mut config,
            vec![draft_rule(RuleAction::Deny, ToolFilter::Any, Some("Read"))]
        )
        .is_err());
        assert_eq!(config, original);
    }

    #[test]
    fn legacy_entries_can_be_reviewed_individually_without_rewriting_other_rules() {
        let mut config: Value = toml::from_str(
            "[permission]\ndeny = ['Mcp(team__*)', 'Webfetch(domain:example.invalid)', 'Any', 'Any(Read)', 'Unknown(/example)', false, 'Read']",
        ).unwrap();
        let original = config.clone();
        let mut rules = rules_from(&config);
        assert_eq!(rules[0].tool, ToolFilter::Mcp);
        assert_eq!(rules[1].tool, ToolFilter::WebFetch);
        assert_eq!(rules[1].pattern_mode, PatternMode::Domain);
        assert_eq!(rules[1].pattern.as_deref(), Some("example.invalid"));
        assert_eq!(rules[2].tool, ToolFilter::Any);
        for rule in &rules[..3] {
            assert!(rule.unparsed.as_ref().unwrap().repairable);
        }
        for rule in &rules[3..6] {
            assert!(!rule.unparsed.as_ref().unwrap().repairable);
        }
        assert!(rules[6].unparsed.is_none());
        apply_rules(&mut config, rules.clone()).unwrap();
        assert_eq!(config, original);

        rules[0].unparsed = None;
        rules.remove(4);
        rules.push(draft_rule(RuleAction::Deny, ToolFilter::Edit, None));
        apply_rules(&mut config, rules).unwrap();

        let saved = config["permission"]["deny"].as_array().unwrap();
        assert_eq!(saved[0].as_str(), Some("MCPTool(team__*)"));
        assert_eq!(
            &saved[1..4],
            &original["permission"]["deny"].as_array().unwrap()[1..4]
        );
        assert_eq!(saved[4].as_bool(), Some(false));
        assert_eq!(saved[5].as_str(), Some("Read"));
        assert_eq!(saved[6].as_str(), Some("Edit"));
    }

    #[test]
    fn malformed_and_mixed_rule_sets_refuse_edits_without_touching_existing_values() {
        for invalid in [
            "[permission]\ndeny = false",
            "[permission]\ndeny = []\n[[permission.rules]]\naction = 'deny'\ntool = 'read'",
            "[[permission.rules]]\ntool = 'read'\npattern = '/example/*'",
        ] {
            let mut config: Value = toml::from_str(invalid).unwrap();
            let original = config.clone();
            assert!(stored_rules(&config).is_err(), "{invalid}");
            assert!(apply_rules(&mut config, Vec::new()).is_err(), "{invalid}");
            assert_eq!(config, original, "{invalid}");
        }
    }

    #[test]
    fn clearing_rules_preserves_unrelated_settings_and_can_be_repeated() {
        let mut config: Value = toml::from_str(
            r#"
                [permission]
                allow = ['Read']
                keep = true
                [models]
                default = 'sample'
            "#,
        )
        .unwrap();
        apply_rules(&mut config, Vec::new()).unwrap();
        assert!(rules_from(&config).is_empty());
        assert_eq!(config["permission"]["keep"].as_bool(), Some(true));
        assert_eq!(config["models"]["default"].as_str(), Some("sample"));
        let saved = config.clone();
        apply_rules(&mut config, Vec::new()).unwrap();
        assert_eq!(config, saved);
    }

    // --- AgentDefaults ---

    #[test]
    fn agent_defaults_default() {
        let d = AgentDefaults::default();
        assert_eq!(d.default_model, "");
        assert_eq!(d.default_permission, "");
        assert_eq!(d.remember_tool_approvals, None);
    }

    // --- PERMISSION_MODES ---

    #[test]
    fn permission_modes_constant() {
        assert_eq!(PERMISSION_MODES, ["ask", "auto", "always-approve"]);
    }

    #[test]
    fn legacy_global_permission_keys_are_removed_without_touching_other_ui_settings() {
        let mut config: Value = toml::from_str(
            r#"
                [ui]
                permission_mode = "always-approve"
                approval_mode = "always-approve"
                yolo = true
                theme = "dark"
            "#,
        )
        .unwrap();

        assert!(remove_legacy_global_permission_mode(&mut config).unwrap());
        let ui = config.get("ui").and_then(Value::as_table).unwrap();
        assert!(!ui.contains_key("permission_mode"));
        assert!(!ui.contains_key("approval_mode"));
        assert!(!ui.contains_key("yolo"));
        assert_eq!(ui.get("theme").and_then(Value::as_str), Some("dark"));
        assert!(!remove_legacy_global_permission_mode(&mut config).unwrap());
    }

    #[test]
    fn runtime_sync_bookkeeping_is_isolated_and_removed_per_session() {
        session_permission_sync().lock().unwrap().clear();
        mark_session_permission_mode_synced("session-a", "auto");
        mark_session_permission_mode_synced("session-b", "always-approve");

        let sessions = session_permission_sync().lock().unwrap().clone();
        assert_eq!(sessions["session-a"].applied_mode.as_deref(), Some("auto"));
        assert_eq!(
            sessions["session-b"].applied_mode.as_deref(),
            Some("always-approve")
        );
        drop(sessions);

        forget_session_permission_mode("session-a");
        let sessions = session_permission_sync().lock().unwrap();
        assert!(!sessions.contains_key("session-a"));
        assert_eq!(
            sessions["session-b"].applied_mode.as_deref(),
            Some("always-approve")
        );
    }

    #[test]
    fn permission_mode_flags_are_mutually_exclusive() {
        assert_eq!(permission_mode_flags("ask"), (false, false));
        assert_eq!(permission_mode_flags("auto"), (false, true));
        assert_eq!(permission_mode_flags("always-approve"), (true, false));
    }

    #[test]
    fn unavailable_auto_mode_falls_back_to_effective_ask_without_losing_configuration() {
        assert_eq!(effective_permission_mode("auto", false, true), "ask");
        assert_eq!(effective_permission_mode("auto", true, true), "auto");
        assert_eq!(
            effective_permission_mode("always-approve", true, false),
            "ask"
        );
    }

    #[test]
    fn unavailable_auto_mode_is_rejected_before_persistence() {
        assert_eq!(
            validate_permission_mode_selection("auto", false, true),
            Err(AUTO_MODE_UNAVAILABLE_REASON.into())
        );
        assert!(validate_permission_mode_selection("auto", true, true).is_ok());
        assert!(validate_permission_mode_selection("ask", false, false).is_ok());
        assert!(validate_permission_mode_selection("always-approve", true, true).is_ok());
        assert!(validate_permission_mode_selection("always-approve", true, false).is_err());
        assert!(validate_permission_mode_selection("invalid", true, true).is_err());
    }

    #[test]
    fn session_creation_requires_the_previewed_mode_when_provided() {
        assert_eq!(
            require_new_session_permission_preview(Ok("auto".into()), Some("auto")),
            Ok("auto".into())
        );
        for resolved in [Ok("always-approve".into()), Err("分类器不可用".into())] {
            assert!(
                require_new_session_permission_preview(resolved, Some("auto"))
                    .unwrap_err()
                    .starts_with("任务默认权限已变化")
            );
        }
        assert_eq!(
            require_new_session_permission_preview(Err("配置无效".into()), None),
            Err("配置无效".into())
        );
    }

    #[tokio::test]
    async fn runtime_notification_carries_mode_flags_and_waits_for_ack() {
        let (client, mut agent) = echo_agent_acp::acp_channels();
        let task = tokio::spawn(async move {
            notify_runtime_permission_mode(&client.tx, "always-approve", None, None, false).await
        });
        let message = agent.rx.recv().await.expect("permission mode notification");
        let echo_agent_acp::AcpAgentMessage::ExtNotification(arguments) = message else {
            panic!("expected ExtNotification")
        };
        assert_eq!(
            arguments.request.method.as_ref(),
            "echo.agent/yolo_mode_changed"
        );
        let params: serde_json::Value =
            serde_json::from_str(arguments.request.params.get()).expect("notification params");
        assert_eq!(params["permission_mode"], "always-approve");
        assert_eq!(params["yolo_mode"], true);
        assert_eq!(params["auto_mode"], false);
        assert_eq!(
            params["clientIdentifier"],
            crate::agent_runtime::DESKTOP_CLIENT_IDENTIFIER
        );
        arguments.response_tx.send(Ok(())).expect("send ack");
        assert!(task.await.expect("notification task").is_ok());
    }

    #[tokio::test]
    async fn runtime_notification_preserves_session_scope() {
        let (client, mut agent) = echo_agent_acp::acp_channels();
        let task = tokio::spawn(async move {
            let included = vec!["automation-session".to_string()];
            let excluded = vec!["interactive-session".to_string()];
            notify_runtime_permission_mode(
                &client.tx,
                "ask",
                Some(&included),
                Some(&excluded),
                false,
            )
            .await
        });
        let message = agent.rx.recv().await.expect("permission mode notification");
        let echo_agent_acp::AcpAgentMessage::ExtNotification(arguments) = message else {
            panic!("expected ExtNotification")
        };
        let params: serde_json::Value =
            serde_json::from_str(arguments.request.params.get()).expect("notification params");
        assert_eq!(
            params["sessionIds"],
            serde_json::json!(["automation-session"])
        );
        assert_eq!(
            params["excludeSessionIds"],
            serde_json::json!(["interactive-session"])
        );
        arguments.response_tx.send(Ok(())).expect("send ack");
        assert!(task.await.expect("notification task").is_ok());
    }
}
