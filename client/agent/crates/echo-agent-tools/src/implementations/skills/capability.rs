//! Optional executable capability contract for a `SKILL.md` package.
//!
//! `SKILL.md` remains compatible with the open prompt-skill format. Packages
//! that also contain [`CAPABILITY_MANIFEST_FILE`] can declare deterministic
//! entrypoints, host requirements, connector/account requirements, permissions,
//! and expected artifacts. The contract is data, not an execution bypass:
//! entrypoints must still be launched through the normal Bash tool so the
//! runtime's sandbox and approval policy remain authoritative.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Read;
use std::path::{Component, Path, PathBuf};

mod render;
mod validate;

use render::render_runtime_contract_from_manifest;
use validate::{command_available, validate_manifest};

pub const CAPABILITY_MANIFEST_FILE: &str = "echo.skill.json";
const MAX_MANIFEST_BYTES: u64 = 256 * 1024;
const MAX_LIST_ITEMS: usize = 64;
const MAX_ACTIONS: usize = 32;
const MAX_TEXT_CHARS: usize = 512;
const MAX_RUNTIME_CONTRACT_BYTES: usize = 4 * 1024;
const MAX_TIMEOUT_SECONDS: u64 = 30 * 60;
const MAX_ARTIFACT_BYTES: u64 = 2 * 1024 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SkillCapabilityManifest {
    pub schema_version: u32,
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(default)]
    pub runtime: Option<SkillRuntime>,
    #[serde(default)]
    pub requirements: SkillRequirements,
    #[serde(default)]
    pub permissions: SkillPermissions,
    #[serde(default)]
    pub artifacts: Vec<SkillArtifact>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SkillRuntime {
    pub kind: SkillRuntimeKind,
    /// Optional command override such as `uv` or `bun`. This must be a command
    /// name, never a path; package code belongs in an entrypoint.
    #[serde(default)]
    pub command: Option<String>,
    pub entrypoints: BTreeMap<String, String>,
    #[serde(default = "default_timeout_seconds")]
    pub timeout_seconds: u64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SkillRuntimeKind {
    Python,
    Node,
    Shell,
}

impl SkillRuntimeKind {
    fn default_command(self) -> &'static str {
        match self {
            Self::Python => "python3",
            Self::Node => "node",
            Self::Shell => {
                if cfg!(windows) {
                    "powershell"
                } else {
                    "sh"
                }
            }
        }
    }
}

fn default_timeout_seconds() -> u64 {
    120
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SkillRequirements {
    #[serde(default)]
    pub commands: Vec<String>,
    #[serde(default)]
    pub connectors: Vec<SkillConnectorRequirement>,
    #[serde(default)]
    pub os_permissions: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SkillConnectorRequirement {
    pub id: String,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub account_required: bool,
    #[serde(default)]
    pub purpose: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SkillPermissions {
    #[serde(default)]
    pub filesystem: SkillFilesystemPermission,
    #[serde(default)]
    pub network: Vec<String>,
    #[serde(default)]
    pub external_actions: Vec<String>,
}

impl Default for SkillPermissions {
    fn default() -> Self {
        Self {
            filesystem: SkillFilesystemPermission::None,
            network: Vec::new(),
            external_actions: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum SkillFilesystemPermission {
    #[default]
    None,
    WorkspaceRead,
    WorkspaceWrite,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SkillArtifact {
    pub id: String,
    /// Package-relative glob-like output pattern. Absolute paths and parent
    /// traversal are forbidden; outputs always belong to the active workspace.
    pub pattern: String,
    #[serde(default)]
    pub mime_type: Option<String>,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub max_bytes: Option<u64>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SkillCapabilityState {
    InstructionOnly,
    Ready,
    MissingDependencies,
    ConfigurationRequired,
    Invalid,
}

impl SkillCapabilityState {
    /// Only prompt-only Skills and fully preflighted executable Skills may run
    /// without a person present. Keep this policy next to the state machine so
    /// every host applies the same fail-closed decision.
    pub fn can_run_automated(self) -> bool {
        matches!(self, Self::InstructionOnly | Self::Ready)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SkillCapabilityCheckStatus {
    Ready,
    Missing,
    ConfigurationRequired,
    Declared,
    Invalid,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SkillCapabilityCheck {
    pub kind: String,
    pub key: String,
    pub status: SkillCapabilityCheckStatus,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SkillCapabilityReport {
    pub declared: bool,
    pub state: SkillCapabilityState,
    pub ready: bool,
    pub capabilities: Vec<String>,
    pub checks: Vec<SkillCapabilityCheck>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manifest: Option<SkillCapabilityManifest>,
}

/// Connector states supplied by the host product. The portable runtime does
/// not own connector credentials, so it leaves these unknown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectorState {
    Ready,
    ConfigurationRequired,
    Missing,
}

/// Load and validate a package's optional capability manifest.
pub fn load_manifest(skill_root: &Path) -> Result<Option<SkillCapabilityManifest>, String> {
    let path = skill_root.join(CAPABILITY_MANIFEST_FILE);
    let metadata = match std::fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("read {CAPABILITY_MANIFEST_FILE}: {error}")),
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(format!("{CAPABILITY_MANIFEST_FILE} must be a regular file"));
    }
    if metadata.len() > MAX_MANIFEST_BYTES {
        return Err(format!("{CAPABILITY_MANIFEST_FILE} exceeds 256 KiB"));
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    let mut file = std::fs::File::open(&path)
        .map_err(|error| format!("read {CAPABILITY_MANIFEST_FILE}: {error}"))?;
    file.by_ref()
        .take(MAX_MANIFEST_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("read {CAPABILITY_MANIFEST_FILE}: {error}"))?;
    if bytes.len() as u64 > MAX_MANIFEST_BYTES {
        return Err(format!("{CAPABILITY_MANIFEST_FILE} exceeds 256 KiB"));
    }
    let manifest: SkillCapabilityManifest = serde_json::from_slice(&bytes)
        .map_err(|error| format!("invalid {CAPABILITY_MANIFEST_FILE}: {error}"))?;
    validate_manifest(skill_root, &manifest)?;
    Ok(Some(manifest))
}

/// Build a host-readiness report. `connector_states=None` means the portable
/// runtime cannot inspect product connector configuration; requirements remain
/// explicitly visible rather than being guessed as ready.
pub fn inspect_capability(
    skill_root: &Path,
    connector_states: Option<&HashMap<String, ConnectorState>>,
) -> SkillCapabilityReport {
    let manifest = match load_manifest(skill_root) {
        Ok(Some(manifest)) => manifest,
        Ok(None) => {
            return SkillCapabilityReport {
                declared: false,
                state: SkillCapabilityState::InstructionOnly,
                ready: true,
                capabilities: Vec::new(),
                checks: vec![SkillCapabilityCheck {
                    kind: "manifest".into(),
                    key: CAPABILITY_MANIFEST_FILE.into(),
                    status: SkillCapabilityCheckStatus::Declared,
                    message: "Prompt-only Skill; no deterministic runtime contract is declared"
                        .into(),
                }],
                manifest: None,
            };
        }
        Err(error) => {
            return SkillCapabilityReport {
                declared: true,
                state: SkillCapabilityState::Invalid,
                ready: false,
                capabilities: Vec::new(),
                checks: vec![SkillCapabilityCheck {
                    kind: "manifest".into(),
                    key: CAPABILITY_MANIFEST_FILE.into(),
                    status: SkillCapabilityCheckStatus::Invalid,
                    message: error,
                }],
                manifest: None,
            };
        }
    };

    let mut checks = Vec::new();
    let mut missing_dependency = false;
    let mut configuration_required = false;

    if let Some(runtime) = &manifest.runtime {
        let command = runtime
            .command
            .as_deref()
            .unwrap_or_else(|| runtime.kind.default_command());
        let present = command_available(command);
        missing_dependency |= !present;
        checks.push(SkillCapabilityCheck {
            kind: "command".into(),
            key: command.into(),
            status: if present {
                SkillCapabilityCheckStatus::Ready
            } else {
                SkillCapabilityCheckStatus::Missing
            },
            message: if present {
                format!("Runtime command `{command}` is available")
            } else {
                format!("Runtime command `{command}` was not found on PATH")
            },
        });
        for (action, entrypoint) in &runtime.entrypoints {
            checks.push(SkillCapabilityCheck {
                kind: "entrypoint".into(),
                key: action.clone(),
                status: SkillCapabilityCheckStatus::Ready,
                message: format!("Action `{action}` uses package entrypoint `{entrypoint}`"),
            });
        }
    }

    let mut seen_commands = HashSet::new();
    for command in &manifest.requirements.commands {
        if !seen_commands.insert(command.as_str()) {
            continue;
        }
        let present = command_available(command);
        missing_dependency |= !present;
        checks.push(SkillCapabilityCheck {
            kind: "command".into(),
            key: command.clone(),
            status: if present {
                SkillCapabilityCheckStatus::Ready
            } else {
                SkillCapabilityCheckStatus::Missing
            },
            message: if present {
                format!("Required command `{command}` is available")
            } else {
                format!("Required command `{command}` was not found on PATH")
            },
        });
    }

    for connector in &manifest.requirements.connectors {
        let state = match connector_states {
            Some(states) => states
                .get(&connector.id)
                .copied()
                .unwrap_or(ConnectorState::Missing),
            None => ConnectorState::ConfigurationRequired,
        };
        let (status, message) = match state {
            ConnectorState::Ready => (
                SkillCapabilityCheckStatus::Ready,
                format!("Connector `{}` is ready", connector_label(connector)),
            ),
            ConnectorState::ConfigurationRequired => {
                configuration_required = true;
                (
                    SkillCapabilityCheckStatus::ConfigurationRequired,
                    if connector.account_required {
                        format!(
                            "Connect an account for `{}` before using this capability",
                            connector_label(connector)
                        )
                    } else {
                        format!("Configure connector `{}`", connector_label(connector))
                    },
                )
            }
            ConnectorState::Missing => {
                missing_dependency = true;
                (
                    SkillCapabilityCheckStatus::Missing,
                    format!(
                        "Required connector `{}` is not installed",
                        connector_label(connector)
                    ),
                )
            }
        };
        checks.push(SkillCapabilityCheck {
            kind: "connector".into(),
            key: connector.id.clone(),
            status,
            message,
        });
    }

    for permission in &manifest.requirements.os_permissions {
        configuration_required = true;
        checks.push(SkillCapabilityCheck {
            kind: "os_permission".into(),
            key: permission.clone(),
            status: SkillCapabilityCheckStatus::ConfigurationRequired,
            message: format!(
                "Operating-system permission `{permission}` must be granted when used"
            ),
        });
    }

    let state = if missing_dependency {
        SkillCapabilityState::MissingDependencies
    } else if configuration_required {
        SkillCapabilityState::ConfigurationRequired
    } else {
        SkillCapabilityState::Ready
    };
    SkillCapabilityReport {
        declared: true,
        state,
        ready: state == SkillCapabilityState::Ready,
        capabilities: manifest.capabilities.clone(),
        checks,
        manifest: Some(manifest),
    }
}

/// Render a bounded, data-only execution contract that is appended to the
/// loaded Skill instructions. This makes the capability contract available on
/// slash expansion, model invocation, and agent-definition preloading alike.
pub fn render_runtime_contract(skill_root: &Path) -> Result<Option<String>, String> {
    let Some(manifest) = load_manifest(skill_root)? else {
        return Ok(None);
    };
    render_runtime_contract_from_manifest(&manifest, command_available).map(Some)
}

/// Append the optional runtime contract without changing prompt-only Skills.
pub fn append_runtime_contract(content: String, skill_path: &Path) -> Result<String, String> {
    let Some(root) = skill_path.parent() else {
        return Ok(content);
    };
    match render_runtime_contract(root)? {
        Some(contract) if content.is_empty() => Ok(contract),
        Some(contract) => Ok(format!("{content}\n\n{contract}")),
        None => Ok(content),
    }
}

fn connector_label(connector: &SkillConnectorRequirement) -> &str {
    connector.label.as_deref().unwrap_or(&connector.id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_manifest(root: &Path, value: serde_json::Value) {
        std::fs::create_dir_all(root.join("scripts")).unwrap();
        std::fs::write(root.join("scripts/run.py"), "print('ok')").unwrap();
        std::fs::write(
            root.join(CAPABILITY_MANIFEST_FILE),
            serde_json::to_vec_pretty(&value).unwrap(),
        )
        .unwrap();
    }

    fn valid_runtime_manifest() -> serde_json::Value {
        serde_json::json!({
            "schemaVersion": 1,
            "capabilities": ["document.create"],
            "runtime": {
                "kind": "python",
                "entrypoints": {"create": "scripts/run.py"}
            }
        })
    }

    #[test]
    fn automated_execution_policy_is_fail_closed() {
        assert!(SkillCapabilityState::InstructionOnly.can_run_automated());
        assert!(SkillCapabilityState::Ready.can_run_automated());
        assert!(!SkillCapabilityState::MissingDependencies.can_run_automated());
        assert!(!SkillCapabilityState::ConfigurationRequired.can_run_automated());
        assert!(!SkillCapabilityState::Invalid.can_run_automated());
    }

    #[test]
    fn validates_and_reports_an_executable_skill() {
        let root = tempfile::tempdir().unwrap();
        write_manifest(
            root.path(),
            serde_json::json!({
                "schemaVersion": 1,
                "capabilities": ["document.docx.create"],
                "runtime": {
                    "kind": "python",
                    "entrypoints": {"create": "scripts/run.py"},
                    "timeoutSeconds": 90
                },
                "permissions": {"filesystem": "workspace-write"},
                "artifacts": [{
                    "id": "document",
                    "pattern": "output/*.docx",
                    "required": true,
                    "maxBytes": 1048576
                }]
            }),
        );
        let manifest = load_manifest(root.path()).unwrap().unwrap();
        assert_eq!(manifest.capabilities, vec!["document.docx.create"]);
        let contract = render_runtime_contract(root.path()).unwrap().unwrap();
        assert!(contract.contains("scripts/run.py"));
        assert!(contract.contains("normal Bash tool"));
        assert!(contract.contains("Verify every required artifact"));
    }

    #[test]
    fn declared_uv_python_runs_in_an_isolated_environment_with_offline_dependencies() {
        let manifest: SkillCapabilityManifest = serde_json::from_value(serde_json::json!({
            "schemaVersion": 1,
            "runtime": { "kind": "python", "command": "uv", "entrypoints": { "create": "scripts/run.py" } }
        })).unwrap();
        let contract =
            render_runtime_contract_from_manifest(&manifest, |command| command == "uv").unwrap();
        let action = contract
            .lines()
            .find_map(|line| line.strip_prefix("action: "))
            .unwrap();
        let action: serde_json::Value = serde_json::from_str(action).unwrap();
        assert_eq!(
            action["arguments"],
            serde_json::json!([
                "run",
                "--isolated",
                "--no-project",
                "--offline",
                "--no-python-downloads",
                "--script",
                "scripts/run.py"
            ])
        );
    }

    #[test]
    fn rejects_missing_entrypoints_and_unsafe_origins() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join(CAPABILITY_MANIFEST_FILE),
            r#"{
              "schemaVersion": 1,
              "capabilities": ["weather.lookup"],
              "runtime": {"kind":"python","entrypoints":{"run":"../escape.py"}},
              "permissions": {"network":["https://user:secret@example.com/path"]}
            }"#,
        )
        .unwrap();
        assert!(
            load_manifest(root.path())
                .unwrap_err()
                .contains("safe relative path")
        );
    }

    #[test]
    fn rejects_unsupported_schema_duplicates_and_empty_contracts() {
        let root = tempfile::tempdir().unwrap();
        for (manifest, expected) in [
            (
                serde_json::json!({
                    "schemaVersion": 2,
                    "capabilities": ["document.create"],
                    "runtime": {"kind": "python", "entrypoints": {"create": "scripts/run.py"}}
                }),
                "schemaVersion",
            ),
            (
                serde_json::json!({
                    "schemaVersion": 1,
                    "capabilities": ["document.create", "document.create"],
                    "runtime": {"kind": "python", "entrypoints": {"create": "scripts/run.py"}}
                }),
                "duplicate capability",
            ),
            (
                serde_json::json!({
                    "schemaVersion": 1,
                    "capabilities": [],
                    "runtime": {"kind": "python", "entrypoints": {"create": "scripts/run.py"}}
                }),
                "at least one capability",
            ),
            (
                serde_json::json!({
                    "schemaVersion": 1,
                    "capabilities": ["document.create"],
                    "runtime": {"kind": "python", "entrypoints": {}}
                }),
                "entrypoints",
            ),
        ] {
            write_manifest(root.path(), manifest);
            assert!(load_manifest(root.path()).unwrap_err().contains(expected));
        }
    }

    #[test]
    fn rejects_each_unsafe_network_origin_form() {
        let root = tempfile::tempdir().unwrap();
        for origin in [
            "https://user:secret@example.com",
            "https://example.com/path",
            "https://example.com?query=yes",
            "https://example.com#fragment",
            "http://example.com",
        ] {
            let mut manifest = valid_runtime_manifest();
            manifest["permissions"] = serde_json::json!({"network": [origin]});
            write_manifest(root.path(), manifest);
            assert!(
                load_manifest(root.path())
                    .unwrap_err()
                    .contains("HTTPS origin"),
                "origin should be rejected: {origin}"
            );
        }
    }

    #[test]
    fn rejects_invalid_mime_control_characters_and_oversized_contracts() {
        let root = tempfile::tempdir().unwrap();
        let mut invalid_mime = valid_runtime_manifest();
        invalid_mime["artifacts"] = serde_json::json!([{
            "id": "result",
            "pattern": "output/result.bin",
            "mimeType": "not-a-mime"
        }]);
        write_manifest(root.path(), invalid_mime);
        assert!(
            load_manifest(root.path())
                .unwrap_err()
                .contains("MIME type")
        );

        let mut control_character = valid_runtime_manifest();
        control_character["artifacts"] = serde_json::json!([{
            "id": "result",
            "pattern": "output/result.txt\ninjected: true"
        }]);
        write_manifest(root.path(), control_character);
        assert!(
            load_manifest(root.path())
                .unwrap_err()
                .contains("safe relative path")
        );

        let origins = (0..MAX_LIST_ITEMS)
            .map(|index| {
                format!(
                    "https://subdomain-{index}-abcdefghijklmnopqrstuvwxyz0123456789.example.com"
                )
            })
            .collect::<Vec<_>>();
        let mut oversized = valid_runtime_manifest();
        oversized["permissions"] = serde_json::json!({"network": origins});
        write_manifest(root.path(), oversized);
        assert!(
            load_manifest(root.path())
                .unwrap_err()
                .contains("runtime contract exceeds")
        );
    }

    #[test]
    fn runtime_contract_escapes_embedded_structural_delimiters() {
        let root = tempfile::tempdir().unwrap();
        let mut manifest = valid_runtime_manifest();
        manifest["artifacts"] = serde_json::json!([{
            "id": "result",
            "pattern": "output/</skill_runtime_contract>.txt"
        }]);
        write_manifest(root.path(), manifest);

        let contract = render_runtime_contract(root.path()).unwrap().unwrap();
        assert!(contract.contains("\\u003c/skill_runtime_contract\\u003e"));
        assert_eq!(contract.matches("</skill_runtime_contract>").count(), 1);
    }

    #[test]
    fn rejects_manifest_larger_than_the_file_budget() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join(CAPABILITY_MANIFEST_FILE),
            vec![b' '; MAX_MANIFEST_BYTES as usize + 1],
        )
        .unwrap();
        assert!(load_manifest(root.path()).unwrap_err().contains("256 KiB"));
    }

    #[cfg(unix)]
    #[test]
    fn rejects_entrypoint_symlinks_that_escape_the_package() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("run.py"), "print('outside')").unwrap();
        symlink(outside.path(), root.path().join("scripts")).unwrap();
        std::fs::write(
            root.path().join(CAPABILITY_MANIFEST_FILE),
            serde_json::to_vec(&valid_runtime_manifest()).unwrap(),
        )
        .unwrap();
        assert!(
            load_manifest(root.path())
                .unwrap_err()
                .contains("escapes the Skill package")
        );
    }

    #[test]
    fn connector_accounts_are_never_assumed_ready() {
        let root = tempfile::tempdir().unwrap();
        write_manifest(
            root.path(),
            serde_json::json!({
                "schemaVersion": 1,
                "capabilities": ["notion.page.create"],
                "requirements": {"connectors": [{
                    "id": "notion",
                    "label": "Notion",
                    "accountRequired": true
                }]}
            }),
        );
        let unknown = inspect_capability(root.path(), None);
        assert_eq!(unknown.state, SkillCapabilityState::ConfigurationRequired);
        assert!(
            unknown
                .checks
                .iter()
                .any(|check| check.message.contains("Connect an account"))
        );

        let ready = HashMap::from([("notion".into(), ConnectorState::Ready)]);
        assert_eq!(
            inspect_capability(root.path(), Some(&ready)).state,
            SkillCapabilityState::Ready
        );

        let missing = inspect_capability(root.path(), Some(&HashMap::new()));
        assert_eq!(missing.state, SkillCapabilityState::MissingDependencies);
        assert!(missing.checks.iter().any(|check| {
            check.kind == "connector" && check.status == SkillCapabilityCheckStatus::Missing
        }));
    }

    #[test]
    fn missing_runtime_command_is_reported_before_execution() {
        let root = tempfile::tempdir().unwrap();
        write_manifest(
            root.path(),
            serde_json::json!({
                "schemaVersion": 1,
                "capabilities": ["document.pdf.create"],
                "runtime": {
                    "kind": "python",
                    "command": "echoagent-command-that-must-not-exist",
                    "entrypoints": {"create": "scripts/run.py"}
                }
            }),
        );

        let report = inspect_capability(root.path(), Some(&HashMap::new()));
        assert_eq!(report.state, SkillCapabilityState::MissingDependencies);
        assert!(!report.ready);
        assert!(report.checks.iter().any(|check| {
            check.kind == "command" && check.status == SkillCapabilityCheckStatus::Missing
        }));
    }

    #[test]
    fn prompt_only_skills_remain_compatible() {
        let root = tempfile::tempdir().unwrap();
        let report = inspect_capability(root.path(), None);
        assert_eq!(report.state, SkillCapabilityState::InstructionOnly);
        assert!(report.ready);
        assert!(render_runtime_contract(root.path()).unwrap().is_none());
    }
}
