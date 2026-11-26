//! Authoritative local policy store and backend enforcement gates.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::{AppHandle, Emitter, State};

use crate::harness::state::HarnessState;

const MAX_POLICY_BYTES: u64 = 1024 * 1024;
const MAX_POLICY_ARRAY_ITEMS: usize = 512;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PolicyRule {
    #[serde(rename = "type")]
    pub rule_type: String,
    pub value: Value,
    #[serde(default)]
    pub priority: i64,
    #[serde(default)]
    pub source: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PolicySet {
    #[serde(default)]
    pub rules: Vec<PolicyRule>,
}

fn policy_path() -> PathBuf {
    crate::paths::echo_agent_home_dir().join("echoagent-policy.json")
}

fn validate_rule(rule: &PolicyRule) -> Result<(), String> {
    match rule.rule_type.as_str() {
        "model-whitelist" | "disabled-features" => {
            let Some(values) = rule.value.as_array() else {
                return Err(format!("{} must be an array", rule.rule_type));
            };
            if values.len() > MAX_POLICY_ARRAY_ITEMS
                || values.iter().any(|value| {
                    value.as_str().is_none_or(|item| {
                        item.is_empty() || item.len() > 512 || item.chars().any(char::is_control)
                    })
                })
            {
                return Err(format!(
                    "{} must contain at most {MAX_POLICY_ARRAY_ITEMS} non-empty strings",
                    rule.rule_type
                ));
            }
        }
        "skill-upload" => {
            if !rule.value.is_boolean() {
                return Err("skill-upload must be boolean".into());
            }
        }
        "permission-mode" => {
            let mode = rule.value.as_str().unwrap_or_default();
            if !["ask", "auto", "always-approve"].contains(&mode) {
                return Err("permission-mode must be ask, auto or always-approve".into());
            }
        }
        "sandbox-rules" | "max-tokens-per-day" => return Err(format!(
            "policy type '{}' is not supported by this build and was not saved; enforcing it only in the UI would create a false security boundary",
            rule.rule_type
        )),
        other => return Err(format!("unknown policy type: {other}")),
    }
    Ok(())
}

fn merge_rules(rules: Vec<PolicyRule>) -> Vec<PolicyRule> {
    let mut merged: HashMap<String, (usize, PolicyRule)> = HashMap::new();
    for (index, rule) in rules.into_iter().enumerate() {
        let replace = merged
            .get(&rule.rule_type)
            .map(|(old_index, old)| {
                rule.priority > old.priority
                    || (rule.priority == old.priority && index >= *old_index)
            })
            .unwrap_or(true);
        if replace {
            merged.insert(rule.rule_type.clone(), (index, rule));
        }
    }
    let mut values: Vec<_> = merged.into_values().collect();
    values.sort_by_key(|(index, _)| *index);
    values.into_iter().map(|(_, rule)| rule).collect()
}

pub fn read_policy() -> Result<PolicySet, String> {
    read_policy_at(&policy_path())
}

fn read_policy_at(path: &Path) -> Result<PolicySet, String> {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(PolicySet::default())
        }
        Err(error) => {
            return Err(format!(
                "无法读取限制策略 {}：{error}；请恢复文件后重试",
                path.display()
            ))
        }
    };
    let mut raw = Vec::new();
    file.take(MAX_POLICY_BYTES + 1)
        .read_to_end(&mut raw)
        .map_err(|error| format!("读取限制策略失败：{error}；受限操作暂时不可用"))?;
    if raw.len() as u64 > MAX_POLICY_BYTES {
        return Err("限制策略超过 1MB，请修复后重试".into());
    }
    let mut set: PolicySet = serde_json::from_slice(&raw).map_err(|error| {
        format!(
            "限制策略损坏（{}）：{error}；请恢复文件后重试",
            path.display()
        )
    })?;
    // These two legacy kinds were never enforcement boundaries.
    set.rules.retain(|rule| {
        !matches!(
            rule.rule_type.as_str(),
            "sandbox-rules" | "max-tokens-per-day"
        )
    });
    for rule in &set.rules {
        validate_rule(rule)?;
    }
    set.rules = merge_rules(set.rules);
    Ok(set)
}

fn write_policy_at(path: &Path, mut set: PolicySet) -> Result<PolicySet, String> {
    for rule in &set.rules {
        validate_rule(rule)?;
    }
    set.rules = merge_rules(set.rules);
    let raw = serde_json::to_vec_pretty(&set).map_err(|e| format!("serialize policy: {e}"))?;
    if raw.len() as u64 > MAX_POLICY_BYTES {
        return Err("限制策略超过 1MB，请减少规则内容后重试".into());
    }
    crate::paths::write_private_file(path, &raw)?;
    Ok(set)
}

fn value(rule_type: &str) -> Result<Option<Value>, String> {
    Ok(read_policy()?
        .rules
        .into_iter()
        .find(|rule| rule.rule_type == rule_type)
        .map(|rule| rule.value))
}

pub fn require_model(model_id: &str) -> Result<(), String> {
    let Some(models) = value("model-whitelist")?.and_then(|v| v.as_array().cloned()) else {
        return Ok(());
    };
    if model_allowed(&models, model_id) {
        Ok(())
    } else {
        Err(format!("策略禁止使用模型 {model_id}"))
    }
}

fn model_allowed(models: &[Value], model_id: &str) -> bool {
    models.iter().any(|value| value.as_str() == Some(model_id))
}

pub fn require_skill_upload() -> Result<(), String> {
    if value("skill-upload")?.and_then(|v| v.as_bool()) == Some(false) {
        Err("策略禁止安装或上传技能".into())
    } else {
        Ok(())
    }
}

pub fn require_feature(feature: &str) -> Result<(), String> {
    let disabled = value("disabled-features")?
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default();
    if disabled.iter().any(|v| v.as_str() == Some(feature)) {
        Err(format!("策略已禁用功能 {feature}"))
    } else {
        Ok(())
    }
}

pub fn locked_permission_mode() -> Option<String> {
    value("permission-mode")
        .unwrap_or_else(|error| {
            tracing::error!(%error, "policy unavailable; enforcing ask mode");
            Some(Value::String("ask".into()))
        })
        .and_then(|v| v.as_str().map(str::to_string))
        .filter(|m| ["ask", "auto", "always-approve"].contains(&m.as_str()))
}

fn permission_policy_requires_runtime_restart(
    previous_locked_mode: Option<&str>,
    current_locked_mode: Option<&str>,
    previous_effective_mode: &str,
    current_effective_mode: &str,
) -> bool {
    previous_locked_mode != current_locked_mode || previous_effective_mode != current_effective_mode
}

#[tauri::command]
pub fn policy_get() -> Result<PolicySet, String> {
    read_policy()
}

#[tauri::command]
pub async fn policy_save(
    app: AppHandle,
    state: State<'_, HarnessState>,
    policy: PolicySet,
) -> Result<PolicySet, String> {
    let _transition_guard = crate::harness::permission_config::permission_transition_lock()
        .lock()
        .await;
    let previous_locked_mode = locked_permission_mode();
    let previous_effective_mode = crate::harness::permission_config::read_permission_mode();
    let saved = write_policy_at(&policy_path(), normalize_local_policy(policy))?;
    let current_locked_mode = locked_permission_mode();
    let current_effective_mode = crate::harness::permission_config::read_permission_mode();
    if permission_policy_requires_runtime_restart(
        previous_locked_mode.as_deref(),
        current_locked_mode.as_deref(),
        &previous_effective_mode,
        &current_effective_mode,
    ) {
        let tx = state.tx.lock().unwrap().clone();
        if let Some(tx) = tx {
            let reason = "权限策略已变更；为确保新策略立即成为安全边界，Agent 已停止，请重新启动";
            if state.mark_runtime_dead_if_current(&tx, reason) {
                let _ = app.emit(
                    "agent://agent-died",
                    serde_json::json!({ "reason": reason }),
                );
            }
        }
    }
    if previous_locked_mode != current_locked_mode {
        let agent_running = state.tx.lock().unwrap().is_some();
        let _ = app.emit(
            "agent://permission-mode",
            crate::harness::permission_config::permission_mode_status(agent_running, None),
        );
    }
    Ok(saved)
}

fn normalize_local_policy(mut policy: PolicySet) -> PolicySet {
    // IPC callers do not get to self-assign an administrator-like source or
    // priority. Managed organization policy has its own signed channel.
    for rule in &mut policy.rules {
        rule.priority = 0;
        rule.source = Some("local-user".into());
    }
    policy
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corrupt_or_unreadable_policy_never_becomes_empty_rules() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("policy.json");
        assert!(read_policy_at(&path).unwrap().rules.is_empty());
        std::fs::write(&path, "{broken").unwrap();
        assert!(read_policy_at(&path).is_err());
        assert!(read_policy_at(dir.path()).is_err());
        std::fs::write(
            &path,
            r#"{"rules":[{"type":"model-whitelist","value":42}]}"#,
        )
        .unwrap();
        assert!(read_policy_at(&path).is_err());
        std::fs::write(
            &path,
            r#"{"rules":[{"type":"model-whitelist","value":[]}]}"#,
        )
        .unwrap();
        assert_eq!(read_policy_at(&path).unwrap().rules.len(), 1);
    }

    #[test]
    fn oversized_serialization_leaves_the_previous_policy_readable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("policy.json");
        let original = normalize_local_policy(PolicySet {
            rules: vec![PolicyRule {
                rule_type: "permission-mode".into(),
                value: Value::String("ask".into()),
                priority: 0,
                source: None,
            }],
        });
        write_policy_at(&path, original).unwrap();
        let before = std::fs::read(&path).unwrap();
        let values = Value::Array(vec![
            Value::String("\\".repeat(512));
            MAX_POLICY_ARRAY_ITEMS
        ]);
        let oversized = normalize_local_policy(PolicySet {
            rules: ["model-whitelist", "disabled-features"]
                .map(|rule_type| PolicyRule {
                    rule_type: rule_type.into(),
                    value: values.clone(),
                    priority: 0,
                    source: None,
                })
                .to_vec(),
        });
        assert!(write_policy_at(&path, oversized)
            .unwrap_err()
            .contains("1MB"));
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert_eq!(read_policy_at(&path).unwrap().rules[0].value, "ask");
    }

    #[test]
    fn adding_permission_lock_restarts_runtime_even_when_effective_mode_is_unchanged() {
        assert!(permission_policy_requires_runtime_restart(
            None,
            Some("ask"),
            "ask",
            "ask"
        ));
    }

    #[test]
    fn unchanged_permission_policy_does_not_restart_runtime() {
        assert!(!permission_policy_requires_runtime_restart(
            Some("auto"),
            Some("auto"),
            "auto",
            "auto"
        ));
    }

    #[test]
    fn merge_uses_priority_then_last_value() {
        let rules = vec![
            PolicyRule {
                rule_type: "skill-upload".into(),
                value: Value::Bool(true),
                priority: 0,
                source: None,
            },
            PolicyRule {
                rule_type: "skill-upload".into(),
                value: Value::Bool(false),
                priority: 2,
                source: None,
            },
            PolicyRule {
                rule_type: "skill-upload".into(),
                value: Value::Bool(true),
                priority: 1,
                source: None,
            },
        ];
        let merged = merge_rules(rules);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].value, Value::Bool(false));
    }

    #[test]
    fn validation_rejects_unknown_and_invalid_values() {
        let invalid = PolicyRule {
            rule_type: "permission-mode".into(),
            value: Value::String("root".into()),
            priority: 0,
            source: None,
        };
        assert!(validate_rule(&invalid).is_err());
        let unknown = PolicyRule {
            rule_type: "mystery".into(),
            value: Value::Null,
            priority: 0,
            source: None,
        };
        assert!(validate_rule(&unknown).is_err());

        for unsupported in ["sandbox-rules", "max-tokens-per-day"] {
            let rule = PolicyRule {
                rule_type: unsupported.into(),
                value: Value::Array(Vec::new()),
                priority: 0,
                source: None,
            };
            assert!(validate_rule(&rule).unwrap_err().contains("not supported"));
        }
    }

    #[test]
    fn renderer_metadata_is_replaced_with_local_provenance() {
        let policy = normalize_local_policy(PolicySet {
            rules: vec![PolicyRule {
                rule_type: "skill-upload".into(),
                value: Value::Bool(true),
                priority: i64::MAX,
                source: Some("administrator".into()),
            }],
        });
        assert_eq!(policy.rules[0].priority, 0);
        assert_eq!(policy.rules[0].source.as_deref(), Some("local-user"));
    }

    #[test]
    fn explicit_empty_model_whitelist_denies_every_model() {
        assert!(!model_allowed(&[], "model-a"));
    }
}
