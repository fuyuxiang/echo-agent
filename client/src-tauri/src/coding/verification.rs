//! Verification engine: detect a project's real build / lint / type-check /
//! test commands, run them, and turn their output into structured records.
//!
//! A verification's pass/fail verdict comes from the process exit code alone.
//! Output parsing only enriches a record with a test summary; when parsing
//! fails the record is still trustworthy, just marked as unstructured.

use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Instant;

use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter, State};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

use crate::coding::{store, task};
use crate::coding_workspace::{high_risk_command_reason, strip_ansi, CodingProcesses};
use crate::shell_fs::FilesystemAccess;

const MAX_OUTPUT_BYTES: usize = 512 * 1024;
const DEFAULT_TIMEOUT_SECS: u64 = 300;

/// Append a line while holding the retained buffer at the cap. A verbose build
/// can emit hundreds of megabytes; the process must still be drained (or it
/// blocks on a full pipe) but memory must not grow with it.
fn push_bounded(buffer: &mut String, line: &str, dropped_early_output: &mut bool) {
    buffer.push_str(line);
    buffer.push('\n');
    if buffer.len() <= MAX_OUTPUT_BYTES {
        return;
    }
    *dropped_early_output = true;
    let overflow = buffer.len() - MAX_OUTPUT_BYTES;
    // Cut on a char boundary at or after the overflow point.
    let cut = buffer
        .char_indices()
        .map(|(index, _)| index)
        .find(|index| *index >= overflow)
        .unwrap_or(buffer.len());
    buffer.drain(..cut);
}

/// Never allocate a whole unbounded line. Preserve UTF-8 across read boundaries.
async fn read_output(mut reader: impl AsyncRead + Unpin, mut emit: impl FnMut(String)) {
    let mut chunk = [0u8; 4096];
    let mut pending = Vec::with_capacity(12288);
    loop {
        let count = reader.read(&mut chunk).await.unwrap_or_default();
        pending.extend_from_slice(&chunk[..count]);
        loop {
            let newline = pending.iter().position(|b| *b == b'\n');
            let mut end = newline.map(|i| i + 1).filter(|i| *i <= 8192).unwrap_or({
                if pending.len() >= 8192 {
                    8192
                } else if count == 0 {
                    pending.len()
                } else {
                    0
                }
            });
            if end == 0 {
                break;
            }
            if let Err(error) = std::str::from_utf8(&pending[..end]) {
                if error.error_len().is_none() && count != 0 {
                    end = error.valid_up_to();
                }
            }
            if end == 0 {
                break;
            }
            let text = String::from_utf8_lossy(&pending[..end])
                .trim_end_matches(['\r', '\n'])
                .to_owned();
            emit(text);
            pending.drain(..end);
        }
        if count == 0 {
            break;
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum VerificationKind {
    Build,
    Lint,
    TypeCheck,
    Test,
    Custom,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum VerificationStatus {
    Running,
    Passed,
    Failed,
    EnvironmentUnavailable,
    TimedOut,
    Cancelled,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TestSummary {
    pub total: u32,
    pub passed: u32,
    pub failed: u32,
    pub skipped: u32,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct DetectedCommand {
    pub kind: VerificationKind,
    pub command: String,
    pub label: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct VerificationRecord {
    pub id: String,
    pub task_id: String,
    pub kind: VerificationKind,
    pub command: String,
    pub status: VerificationStatus,
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub duration_ms: u64,
    pub started_at: String,
    pub finished_at: String,
    #[serde(default)]
    pub content_revision: Option<String>,
    #[serde(default)]
    pub configuration_revision: Option<String>,
    #[serde(default)]
    pub workspace_tree: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    pub test_summary: Option<TestSummary>,
    /// False when the output could not be parsed into a summary, so the UI can
    /// say so instead of implying a clean structured result.
    pub structured: bool,
}

fn records_path(root: &Path, task_id: &str) -> PathBuf {
    store::task_dir(root, task_id).join("verifications.jsonl")
}

pub fn append_record(root: &Path, record: &VerificationRecord) -> Result<(), String> {
    store::append_jsonl(&records_path(root, &record.task_id), record)
}

pub fn list_records(root: &Path, task_id: &str) -> Vec<VerificationRecord> {
    store::read_jsonl(&records_path(root, task_id))
}

/// History is append-only. A new command run never erases another command's
/// evidence, including older revisions that remain useful for diagnosis.
pub fn clear_records(_root: &Path, _task_id: &str) -> Result<(), String> {
    Ok(())
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ConfiguredCheck {
    pub id: String,
    pub kind: VerificationKind,
    pub command: String,
    pub label: String,
    #[serde(default)]
    pub cwd: String,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub timeout_secs: Option<u64>,
    #[serde(default)]
    pub auto_run: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct VerificationProjectConfig {
    #[serde(default)]
    pub checks: Vec<ConfiguredCheck>,
}

pub fn project_config(root: &Path) -> VerificationProjectConfig {
    store::read_json(&store::workspace_dir(root).join("verification-config.json"))
        .unwrap_or_default()
}

pub fn configuration_revision(root: &Path) -> String {
    configuration_revision_for(&project_config(root))
}

fn configuration_revision_for(config: &VerificationProjectConfig) -> String {
    let mut hasher = Sha256::new();
    hasher.update(serde_json::to_vec(config).unwrap_or_default());
    // Relevant environment changes expire evidence without persisting secrets.
    for key in [
        "PATH",
        "NODE_ENV",
        "CI",
        "RUSTFLAGS",
        "JAVA_HOME",
        "VIRTUAL_ENV",
    ] {
        hasher.update(key.as_bytes());
        hasher.update(
            std::env::var_os(key)
                .unwrap_or_default()
                .to_string_lossy()
                .as_bytes(),
        );
    }
    format!("{:x}", hasher.finalize())
}

pub fn current_records(
    root: &Path,
    task_id: &str,
    set: &crate::coding::changeset::ChangeSet,
) -> Vec<VerificationRecord> {
    let revision = set.content_revision();
    let configuration = configuration_revision(root);
    list_records(root, task_id)
        .into_iter()
        .filter(|record| {
            record.content_revision.as_deref() == Some(revision.as_str())
                && record.configuration_revision.as_deref() == Some(configuration.as_str())
        })
        .collect()
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct VerificationFreshness {
    pub content_revision: String,
    pub configuration_revision: String,
    pub workspace_tree: Option<String>,
}

#[tauri::command]
pub async fn coding_verification_freshness(
    access: State<'_, FilesystemAccess>,
    root: String,
    task_id: String,
) -> Result<VerificationFreshness, String> {
    let root = access.require_workspace(&root)?;
    let set = crate::coding::changeset::sync_changes(&root, &task_id).await?;
    Ok(VerificationFreshness {
        content_revision: set.content_revision(),
        configuration_revision: configuration_revision(&root),
        workspace_tree: workspace_tree(&root).await,
    })
}

#[tauri::command]
pub async fn coding_verification_config_get(
    access: State<'_, FilesystemAccess>,
    root: String,
) -> Result<VerificationProjectConfig, String> {
    let root = access.require_workspace(&root)?;
    Ok(project_config(&root))
}

#[tauri::command]
pub async fn coding_verification_config_save(
    access: State<'_, FilesystemAccess>,
    root: String,
    config: VerificationProjectConfig,
) -> Result<VerificationProjectConfig, String> {
    let root = access.require_workspace(&root)?;
    if config.checks.len() > 32 {
        return Err("最多配置 32 项项目检查".into());
    }
    let mut ids = std::collections::HashSet::new();
    let mut commands = std::collections::HashSet::new();
    for check in &config.checks {
        if check
            .timeout_secs
            .is_some_and(|timeout| !(1..=3600).contains(&timeout))
        {
            return Err("检查超时必须为 1～3600 秒的整数".into());
        }
        if check.id.trim().is_empty()
            || !ids.insert(&check.id)
            || check.command.trim().is_empty()
            || check.command.len() > 4096
            || !commands.insert(&check.command)
        {
            return Err("检查 ID 和命令不能为空或重复".into());
        }
        let cwd = if check.cwd.is_empty() || check.cwd == "." {
            root.clone()
        } else {
            crate::coding::changeset::resolve_in_workspace(&root, &check.cwd)?
        };
        let canonical = cwd
            .canonicalize()
            .map_err(|error| format!("检查目录不可用：{error}"))?;
        if !canonical.starts_with(&root) || !canonical.is_dir() {
            return Err("检查目录必须位于当前工程".into());
        }
        if check.env.keys().any(|key| {
            let key = key.to_ascii_uppercase();
            [
                "TOKEN",
                "PASSWORD",
                "SECRET",
                "CREDENTIAL",
                "API_KEY",
                "PRIVATE_KEY",
            ]
            .iter()
            .any(|sensitive| key.contains(sensitive))
        }) {
            return Err(
                "检查配置仅保存非敏感环境覆盖；密钥请使用现有模型/安全配置机制或继承进程环境"
                    .into(),
            );
        }
        if check
            .env
            .iter()
            .any(|(key, value)| key.is_empty() || key.contains(['=', '\0']) || value.contains('\0'))
        {
            return Err("环境覆盖包含无效的名称或值".into());
        }
    }
    store::write_json(
        &store::workspace_dir(&root).join("verification-config.json"),
        &config,
    )?;
    Ok(config)
}

/// Tree identity for the disk content consumed by checks. The private index
/// preserves the user's staging area and respects normal Git ignore rules.
pub async fn workspace_tree(root: &Path) -> Option<String> {
    let dir = tempfile::tempdir().ok()?;
    let index = dir.path().join("index");
    let objects = dir.path().join("objects");
    std::fs::create_dir(&objects).ok()?;
    let location = crate::process_supervisor::background_async_command("git")
        .args(["rev-parse", "--git-path", "objects"])
        .current_dir(root)
        .output()
        .await
        .ok()?;
    if !location.status.success() {
        return None;
    }
    let location = PathBuf::from(String::from_utf8_lossy(&location.stdout).trim());
    let original_objects = if location.is_absolute() {
        location
    } else {
        root.join(location)
    };
    let alternate = std::env::join_paths([original_objects]).ok()?;
    // Fingerprinting never runs configured clean/process filters and never
    // writes objects or index metadata into the user's repository. Git
    // candidates transformed by filters consequently require separate proof.
    let mut overrides = crate::coding_workspace::read_only_git_overrides(root).ok()?;
    overrides.extend(["-c".into(), "core.splitIndex=false".into()]);
    let mut result = None;
    for arguments in [
        vec!["read-tree", "HEAD"],
        vec!["add", "-A", "--", "."],
        vec!["write-tree"],
    ] {
        let output = crate::process_supervisor::background_async_command("git")
            .args(&overrides)
            .args(arguments)
            .env("GIT_INDEX_FILE", &index)
            .env("GIT_OBJECT_DIRECTORY", &objects)
            .env("GIT_ALTERNATE_OBJECT_DIRECTORIES", &alternate)
            .env("GIT_OPTIONAL_LOCKS", "0")
            .current_dir(root)
            .output()
            .await
            .ok()?;
        if !output.status.success() {
            return None;
        }
        result = Some(String::from_utf8_lossy(&output.stdout).trim().to_string());
    }
    result
}

fn label_for(kind: VerificationKind) -> &'static str {
    match kind {
        VerificationKind::Build => "构建",
        VerificationKind::Lint => "静态检查",
        VerificationKind::TypeCheck => "类型检查",
        VerificationKind::Test => "测试",
        VerificationKind::Custom => "命令",
    }
}

fn workspace_has_script(root: &Path, script: &str) -> bool {
    walkdir::WalkDir::new(root)
        .max_depth(5)
        .into_iter()
        .filter_entry(|entry| {
            entry.depth() == 0
                || !matches!(
                    entry.file_name().to_string_lossy().as_ref(),
                    "node_modules" | ".git" | "target" | "dist" | "build"
                )
        })
        .filter_map(Result::ok)
        .filter(|entry| {
            entry.file_name() == "package.json" && entry.path() != root.join("package.json")
        })
        .any(|entry| {
            std::fs::read_to_string(entry.path())
                .ok()
                .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
                .and_then(|manifest| {
                    manifest
                        .get("scripts")
                        .and_then(|value| value.as_object())
                        .map(|scripts| scripts.contains_key(script))
                })
                .unwrap_or(false)
        })
}

/// Detect the project's real verification commands from its manifests. Only
/// commands that actually exist are returned, so the orchestrator never invents
/// a script the project does not define.
pub fn detect_commands(root: &Path) -> Vec<DetectedCommand> {
    let configured = project_config(root);
    if !configured.checks.is_empty() {
        return configured
            .checks
            .into_iter()
            .map(|check| DetectedCommand {
                kind: check.kind,
                command: check.command,
                label: check.label,
            })
            .collect();
    }
    detect_commands_for_platform(root, cfg!(windows))
}

fn detect_commands_for_platform(root: &Path, windows: bool) -> Vec<DetectedCommand> {
    let mut detected: Vec<DetectedCommand> = Vec::new();
    let push = |detected: &mut Vec<DetectedCommand>, kind: VerificationKind, command: String| {
        if !detected.iter().any(|entry| entry.command == command) {
            detected.push(DetectedCommand {
                kind,
                label: label_for(kind).to_string(),
                command,
            });
        }
    };

    if let Some(manifest) = std::fs::read_to_string(root.join("package.json"))
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
    {
        let runner =
            if root.join("pnpm-lock.yaml").exists() || root.join("pnpm-workspace.yaml").exists() {
                "pnpm"
            } else if root.join("yarn.lock").exists() {
                "yarn"
            } else {
                "npm run"
            };
        if let Some(scripts) = manifest.get("scripts").and_then(|value| value.as_object()) {
            for (name, kind) in [
                ("build", VerificationKind::Build),
                ("lint", VerificationKind::Lint),
                ("typecheck", VerificationKind::TypeCheck),
                ("type-check", VerificationKind::TypeCheck),
                ("tsc", VerificationKind::TypeCheck),
                ("test", VerificationKind::Test),
            ] {
                if scripts.contains_key(name) {
                    push(&mut detected, kind, format!("{runner} {name}"));
                }
            }
        }
        let has_workspaces =
            root.join("pnpm-workspace.yaml").exists() || manifest.get("workspaces").is_some();
        if has_workspaces {
            for (name, kind) in [
                ("build", VerificationKind::Build),
                ("lint", VerificationKind::Lint),
                ("typecheck", VerificationKind::TypeCheck),
                ("type-check", VerificationKind::TypeCheck),
                ("test", VerificationKind::Test),
            ] {
                if !workspace_has_script(root, name) {
                    continue;
                }
                let command = match runner {
                    "pnpm" => format!("pnpm -r --if-present run {name}"),
                    "yarn" => format!("yarn workspaces run {name}"),
                    _ => format!("npm run {name} --workspaces --if-present"),
                };
                push(&mut detected, kind, command);
            }
        }
    }

    if root.join("Cargo.toml").exists() {
        push(&mut detected, VerificationKind::Build, "cargo build".into());
        push(&mut detected, VerificationKind::Lint, "cargo clippy".into());
        push(&mut detected, VerificationKind::Test, "cargo test".into());
    }
    if root.join("pom.xml").exists() {
        let maven = if windows && root.join("mvnw.cmd").is_file() {
            r".\mvnw.cmd"
        } else if !windows && root.join("mvnw").is_file() {
            "./mvnw"
        } else {
            "mvn"
        };
        push(
            &mut detected,
            VerificationKind::Build,
            format!("{maven} -B compile"),
        );
        push(
            &mut detected,
            VerificationKind::Test,
            format!("{maven} -B test"),
        );
    }
    if root.join("build.gradle").exists() || root.join("build.gradle.kts").exists() {
        let gradle = if windows && root.join("gradlew.bat").is_file() {
            r".\gradlew.bat"
        } else if !windows && root.join("gradlew").is_file() {
            "./gradlew"
        } else {
            "gradle"
        };
        push(
            &mut detected,
            VerificationKind::Build,
            format!("{gradle} build"),
        );
        push(
            &mut detected,
            VerificationKind::Test,
            format!("{gradle} test"),
        );
    }
    let pyproject = std::fs::read_to_string(root.join("pyproject.toml"))
        .ok()
        .and_then(|text| text.parse::<toml::Value>().ok())
        .unwrap_or(toml::Value::Table(Default::default()));
    let tools = pyproject.get("tool");
    let runner = if root.join("uv.lock").is_file() || tools.and_then(|v| v.get("uv")).is_some() {
        "uv run --no-sync python -m"
    } else if root.join("poetry.lock").is_file() || tools.and_then(|v| v.get("poetry")).is_some() {
        "poetry run python -m"
    } else if windows && root.join(".venv/Scripts/python.exe").is_file() {
        r".\.venv\Scripts\python.exe -m"
    } else if !windows && root.join(".venv/bin/python").is_file() {
        "./.venv/bin/python -m"
    } else if windows {
        "py -m"
    } else {
        "python3 -m"
    };
    let requirement_files = [
        "requirements.txt",
        "requirements-dev.txt",
        "requirements-test.txt",
    ];
    for (name, kind, args, config_files) in [
        ("pytest", VerificationKind::Test, "", vec!["pytest.ini"]),
        (
            "mypy",
            VerificationKind::TypeCheck,
            " .",
            vec!["mypy.ini", ".mypy.ini"],
        ),
    ] {
        let matcher = regex::Regex::new(&format!(r"(?i)^{}(?:$|[\[<>=!~;\s])", name))
            .expect("dependency pattern");
        let declared = tools.and_then(|v| v.get(name)).is_some()
            || config_files.iter().any(|file| root.join(file).is_file())
            || requirement_files.iter().any(|file| {
                std::fs::read_to_string(root.join(file))
                    .ok()
                    .is_some_and(|text| text.lines().any(|line| matcher.is_match(line.trim())))
            })
            || [
                pyproject.get("project").and_then(|v| v.get("dependencies")),
                pyproject
                    .get("project")
                    .and_then(|v| v.get("optional-dependencies")),
                pyproject.get("dependency-groups"),
                tools.and_then(|v| v.get("poetry")),
            ]
            .into_iter()
            .flatten()
            .any(|value| declares_python_dependency(value, name, &matcher));
        if declared {
            push(&mut detected, kind, format!("{runner} {name}{args}"));
        }
    }
    if root.join("go.mod").exists() {
        push(
            &mut detected,
            VerificationKind::Build,
            "go build ./...".into(),
        );
        push(&mut detected, VerificationKind::Lint, "go vet ./...".into());
        push(
            &mut detected,
            VerificationKind::Test,
            "go test ./...".into(),
        );
    }
    if root.join("CMakeLists.txt").exists() {
        push(
            &mut detected,
            VerificationKind::Build,
            "cmake -S . -B build && cmake --build build".into(),
        );
        push(
            &mut detected,
            VerificationKind::Test,
            "ctest --test-dir build --output-on-failure".into(),
        );
    }
    if root.join("MODULE.bazel").exists() || root.join("WORKSPACE").exists() {
        push(
            &mut detected,
            VerificationKind::Test,
            "bazel test //...".into(),
        );
    }
    let has_dotnet_project = std::fs::read_dir(root).ok().is_some_and(|entries| {
        entries.filter_map(Result::ok).any(|entry| {
            matches!(
                entry.path().extension().and_then(|value| value.to_str()),
                Some("sln" | "csproj" | "fsproj")
            )
        })
    });
    if has_dotnet_project {
        push(
            &mut detected,
            VerificationKind::Build,
            "dotnet build".into(),
        );
        push(
            &mut detected,
            VerificationKind::Test,
            "dotnet test --no-build".into(),
        );
    }
    detected
}

fn declares_python_dependency(value: &toml::Value, name: &str, matcher: &regex::Regex) -> bool {
    match value {
        toml::Value::String(value) => matcher.is_match(value),
        toml::Value::Array(values) => values
            .iter()
            .any(|v| declares_python_dependency(v, name, matcher)),
        toml::Value::Table(values) => values
            .iter()
            .any(|(key, value)| key == name || declares_python_dependency(value, name, matcher)),
        _ => false,
    }
}

fn is_manifest_verification(root: &Path, command: &str) -> bool {
    detect_commands(root)
        .iter()
        .any(|detected| detected.command == command)
}

fn is_planned_verification(task: &task::CodingTask, command: &str) -> bool {
    task.task_nodes.iter().any(|node| {
        node.verification_commands
            .iter()
            .any(|planned| planned == command)
    })
}

fn capture(text: &str, pattern: &str, group: usize) -> Option<u32> {
    regex::Regex::new(pattern)
        .ok()?
        .captures(text)?
        .get(group)?
        .as_str()
        .parse()
        .ok()
}

/// Parse a test runner's summary line. Returns `None` when no known shape is
/// present; callers must not treat that as zero tests.
pub fn parse_test_output(output: &str) -> Option<TestSummary> {
    // JUnit / Maven: "Tests run: 24, Failures: 2, Errors: 1, Skipped: 3"
    if let Some(total) = capture(output, r"Tests run:\s*(\d+)", 1) {
        let failures = capture(output, r"Failures:\s*(\d+)", 1).unwrap_or(0);
        let errors = capture(output, r"Errors:\s*(\d+)", 1).unwrap_or(0);
        let skipped = capture(output, r"Skipped:\s*(\d+)", 1).unwrap_or(0);
        let failed = failures + errors;
        return Some(TestSummary {
            total,
            failed,
            skipped,
            passed: total.saturating_sub(failed + skipped),
        });
    }
    // cargo: "test result: FAILED. 8 passed; 2 failed; 1 ignored"
    if output.contains("test result:") {
        let passed = capture(output, r"(\d+)\s+passed", 1).unwrap_or(0);
        let failed = capture(output, r"(\d+)\s+failed", 1).unwrap_or(0);
        let skipped = capture(output, r"(\d+)\s+ignored", 1).unwrap_or(0);
        return Some(TestSummary {
            total: passed + failed + skipped,
            passed,
            failed,
            skipped,
        });
    }
    // vitest prints two tallies — "Test Files 2 passed" then "Tests 42 passed".
    // Prefer the case-level line so the file count is never mistaken for it.
    let case_line = output
        .lines()
        .find(|line| {
            let trimmed = line.trim_start();
            trimmed.starts_with("Tests ") && !trimmed.starts_with("Test Files")
        })
        .map(|line| line.to_string());
    let scope = case_line.as_deref().unwrap_or(output);

    // pytest / vitest: "3 failed, 12 passed, 2 skipped"
    if let Some(passed) = capture(scope, r"(\d+)\s+passed", 1) {
        let failed = capture(scope, r"(\d+)\s+failed", 1).unwrap_or(0);
        let skipped = capture(scope, r"(\d+)\s+skipped", 1).unwrap_or(0);
        return Some(TestSummary {
            total: passed + failed + skipped,
            passed,
            failed,
            skipped,
        });
    }
    None
}

/// Build a record from finished process parts. Extracted so the exit-code-only
/// verdict rule is unit-testable without spawning a process.
#[allow(clippy::too_many_arguments)]
pub fn record_from_parts(
    task_id: &str,
    kind: VerificationKind,
    command: &str,
    exit_code: Option<i32>,
    stdout: String,
    stderr: String,
    duration_ms: u64,
    timed_out: bool,
    cancelled: bool,
) -> VerificationRecord {
    let status = if cancelled {
        VerificationStatus::Cancelled
    } else if timed_out {
        VerificationStatus::TimedOut
    } else if exit_code == Some(0) {
        VerificationStatus::Passed
    } else if matches!(exit_code, Some(127 | 9009))
        || stderr.contains("No module named pytest")
        || stderr.contains("No module named mypy")
        || stderr.contains("No module named 'pytest'")
        || stderr.contains("No module named 'mypy'")
    {
        VerificationStatus::EnvironmentUnavailable
    } else {
        VerificationStatus::Failed
    };
    let combined = format!("{stdout}\n{stderr}");
    let test_summary = matches!(kind, VerificationKind::Test)
        .then(|| parse_test_output(&combined))
        .flatten();
    VerificationRecord {
        id: uuid::Uuid::now_v7().to_string(),
        task_id: task_id.to_string(),
        kind,
        command: command.to_string(),
        status,
        exit_code,
        stdout,
        stderr,
        duration_ms,
        started_at: chrono::Utc::now().to_rfc3339(),
        finished_at: chrono::Utc::now().to_rfc3339(),
        content_revision: None,
        configuration_revision: None,
        workspace_tree: None,
        cwd: None,
        structured: test_summary.is_some(),
        test_summary,
    }
}

fn label_dropped(text: String, dropped_early_output: bool) -> String {
    if dropped_early_output {
        format!("…较早输出已省略…\n{text}")
    } else {
        text
    }
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct VerificationChunk {
    root: String,
    task_id: String,
    run_id: String,
    stream: &'static str,
    chunk: String,
}

/// Payload shape the pre-workbench coding UI listens for on
/// `coding://command-output`. Emitted alongside the workbench event so both UIs
/// see live output from the single execution path.
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct LegacyChunk {
    run_id: String,
    stream: &'static str,
    data: String,
}

fn emit_output(
    app: &AppHandle,
    root: &Path,
    task_id: &str,
    run_id: &str,
    stream: &'static str,
    line: &str,
) {
    let chunk = format!("{line}\n");
    let _ = app.emit(
        "coding://verification-output",
        VerificationChunk {
            root: root.to_string_lossy().into_owned(),
            task_id: task_id.to_string(),
            run_id: run_id.to_string(),
            stream,
            chunk: chunk.clone(),
        },
    );
    let _ = app.emit(
        "coding://command-output",
        LegacyChunk {
            run_id: run_id.to_string(),
            stream,
            data: chunk,
        },
    );
}

trait VerificationEvents: Send + Sync {
    fn output(&self, root: &Path, task_id: &str, run_id: &str, stream: &'static str, line: &str);
    fn updated(&self, record: &VerificationRecord);
}

struct AppVerificationEvents(AppHandle);

impl VerificationEvents for AppVerificationEvents {
    fn output(&self, root: &Path, task_id: &str, run_id: &str, stream: &'static str, line: &str) {
        emit_output(&self.0, root, task_id, run_id, stream, line);
    }

    fn updated(&self, record: &VerificationRecord) {
        let _ = self.0.emit("coding://verification-updated", record);
    }
}

/// Run one verification command inside the workspace. Streams output to the UI
/// and honours the same native high-risk command policy as the rest of the app.
///
/// `requested_run_id` lets a caller supply the id the run registers under: the
/// legacy coding UI generates its own id up front and cancels by it, so that id
/// has to be the one we track. Pass `None` to have one generated.
#[allow(clippy::too_many_arguments)]
pub async fn run(
    app: AppHandle,
    processes: &CodingProcesses,
    root: PathBuf,
    task_id: String,
    kind: VerificationKind,
    command: String,
    timeout_secs: Option<u64>,
    requested_run_id: Option<String>,
    approval_token: Option<String>,
) -> Result<VerificationRecord, String> {
    run_with_events(
        Arc::new(AppVerificationEvents(app)),
        processes,
        root,
        task_id,
        kind,
        command,
        timeout_secs,
        requested_run_id,
        approval_token,
    )
    .await
}

// Keep process execution, consent, persistence and cleanup in one path. The
// event destination is the only seam used by subprocess tests without a GUI.
#[allow(clippy::too_many_arguments)]
async fn run_with_events(
    events: Arc<dyn VerificationEvents>,
    processes: &CodingProcesses,
    root: PathBuf,
    task_id: String,
    kind: VerificationKind,
    command: String,
    timeout_secs: Option<u64>,
    requested_run_id: Option<String>,
    approval_token: Option<String>,
) -> Result<VerificationRecord, String> {
    crate::coding_document_bridge::ensure_workspace_admission(&root.to_string_lossy()).await?;
    store::validate_task_id(&task_id)?;
    let coding_task = task::load(&root, &task_id).ok_or_else(|| "任务不存在".to_string())?;
    let command_text = command.trim().to_string();
    if command_text.is_empty() || command_text.len() > 4_096 {
        return Err("命令不能为空".into());
    }
    if let Some(reason) = high_risk_command_reason(&command_text, &root) {
        return Err(format!("命令被原生安全策略拒绝：{reason}"));
    }
    let configuration_snapshot = project_config(&root);
    let check = configuration_snapshot
        .checks
        .iter()
        .find(|check| check.command == command_text)
        .cloned();
    if check.is_none()
        && (is_planned_verification(&coding_task, &command_text)
            || !is_manifest_verification(&root, &command_text))
    {
        let token = approval_token
            .as_deref()
            .ok_or_else(|| "该命令来自执行计划，运行前需要用户确认".to_string())?;
        processes.consume_verification_approval(token, &root, &task_id, &command_text)?;
    }
    let content_revision = crate::coding::changeset::sync_changes(&root, &task_id)
        .await?
        .content_revision();
    let configuration = configuration_revision_for(&configuration_snapshot);
    let tree = workspace_tree(&root).await;
    let cwd = match check.as_ref().map(|check| check.cwd.as_str()) {
        Some(value) if !value.is_empty() && value != "." => {
            crate::coding::changeset::resolve_in_workspace(&root, value)?
        }
        _ => root.clone(),
    };
    let cwd = cwd
        .canonicalize()
        .map_err(|error| format!("检查目录不可用：{error}"))?;
    if !cwd.starts_with(&root) {
        return Err("检查目录必须位于当前工程".into());
    }
    let run_id = requested_run_id
        .filter(|value| {
            !value.is_empty()
                && value.len() <= 100
                && value.chars().all(|character| {
                    character.is_ascii_alphanumeric() || matches!(character, '-' | '_')
                })
        })
        .unwrap_or_else(|| uuid::Uuid::now_v7().to_string());
    let cancellation = CancellationToken::new();
    processes.register(&run_id, cancellation.clone())?;

    let mut builder = if cfg!(target_os = "windows") {
        let mut builder = Command::new("cmd");
        builder.args(["/D", "/S", "/C", &command_text]);
        builder
    } else {
        let mut builder = Command::new("sh");
        builder.args(["-lc", &command_text]);
        builder
    };
    builder
        .current_dir(&cwd)
        .env("CI", "true")
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if let Some(check) = &check {
        builder.envs(&check.env);
    }
    let kind = check.as_ref().map(|check| check.kind).unwrap_or(kind);

    let started_at = chrono::Utc::now().to_rfc3339();
    let started = Instant::now();
    let mut child = crate::process_supervisor::spawn_async(builder).map_err(|error| {
        processes.unregister(&run_id);
        format!("无法执行命令：{error}")
    })?;
    let stdout = child.stdout().take().ok_or("无法捕获标准输出")?;
    let stderr = child.stderr().take().ok_or("无法捕获错误输出")?;

    let stdout_buffer = Arc::new(Mutex::new((String::new(), false)));
    let stderr_buffer = Arc::new(Mutex::new((String::new(), false)));
    let spawn_reader = |stream: Box<dyn AsyncRead + Unpin + Send>,
                        channel: &'static str,
                        buffer: Arc<Mutex<(String, bool)>>| {
        let events = events.clone();
        let root = root.clone();
        let task_id = task_id.clone();
        let run_id = run_id.clone();
        tokio::spawn(async move {
            read_output(stream, |raw| {
                let line = strip_ansi(raw);
                events.output(&root, &task_id, &run_id, channel, &line);
                let mut guard = buffer.lock().unwrap_or_else(|e| e.into_inner());
                let (text, dropped) = &mut *guard;
                push_bounded(text, &line, dropped);
            })
            .await;
        })
    };
    let stdout_task = spawn_reader(Box::new(stdout), "stdout", stdout_buffer.clone());
    let stderr_task = spawn_reader(Box::new(stderr), "stderr", stderr_buffer.clone());

    let timeout = std::time::Duration::from_secs(
        timeout_secs
            .or_else(|| check.as_ref().and_then(|check| check.timeout_secs))
            .unwrap_or(DEFAULT_TIMEOUT_SECS)
            .clamp(1, 3_600),
    );
    let mut timed_out = false;
    let mut cancelled = false;
    let exit_status = tokio::select! {
        status = child.wait() => status.ok(),
        _ = tokio::time::sleep(timeout) => {
            timed_out = true;
            crate::process_supervisor::stop_async(&mut child).await
        }
        _ = cancellation.cancelled() => {
            cancelled = true;
            crate::process_supervisor::stop_async(&mut child).await
        }
    };

    // Reclaim detached background workers even when the command exited normally.
    let _ = child.start_kill();
    for mut reader in [stdout_task, stderr_task] {
        if tokio::time::timeout(std::time::Duration::from_secs(2), &mut reader)
            .await
            .is_err()
        {
            reader.abort();
            let _ = reader.await;
        }
    }
    processes.unregister(&run_id);
    let (stdout_buffer, stdout_dropped) = stdout_buffer
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    let (stderr_buffer, stderr_dropped) = stderr_buffer
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    let stdout_text = label_dropped(stdout_buffer, stdout_dropped);
    let stderr_text = label_dropped(stderr_buffer, stderr_dropped);
    let mut record = record_from_parts(
        &task_id,
        kind,
        &command_text,
        exit_status.and_then(|status| status.code()),
        stdout_text,
        stderr_text,
        started.elapsed().as_millis() as u64,
        timed_out,
        cancelled,
    );
    record.started_at = started_at;
    record.content_revision = Some(content_revision);
    record.configuration_revision = Some(configuration);
    record.workspace_tree = tree;
    record.cwd = Some(cwd.to_string_lossy().into_owned());
    let write_root = root.clone();
    let write_record = record.clone();
    tokio::task::spawn_blocking(move || append_record(&write_root, &write_record))
        .await
        .map_err(|error| format!("写入验证记录失败：{error}"))??;
    events.updated(&record);
    Ok(record)
}

#[tauri::command]
pub async fn coding_verification_detect(
    access: State<'_, FilesystemAccess>,
    root: String,
) -> Result<Vec<DetectedCommand>, String> {
    let root = access.require_workspace(&root)?;
    tokio::task::spawn_blocking(move || detect_commands(&root))
        .await
        .map_err(|error| format!("识别验证命令失败：{error}"))
}

#[tauri::command]
pub async fn coding_verification_list(
    access: State<'_, FilesystemAccess>,
    root: String,
    task_id: String,
) -> Result<Vec<VerificationRecord>, String> {
    let root = access.require_workspace(&root)?;
    store::validate_task_id(&task_id)?;
    tokio::task::spawn_blocking(move || list_records(&root, &task_id))
        .await
        .map_err(|error| format!("读取验证记录失败：{error}"))
}

#[tauri::command]
pub async fn coding_verification_approve_plan_command(
    access: State<'_, FilesystemAccess>,
    processes: State<'_, CodingProcesses>,
    root: String,
    task_id: String,
    command: String,
) -> Result<String, String> {
    let root = access.require_workspace(&root)?;
    store::validate_task_id(&task_id)?;
    if task::load(&root, &task_id).is_none() {
        return Err("任务不存在".into());
    }
    let command = command.trim();
    if command.is_empty() || command.len() > 4_096 {
        return Err("验证命令无效".into());
    }
    if let Some(reason) = high_risk_command_reason(command, &root) {
        return Err(format!("命令被原生安全策略拒绝：{reason}"));
    }
    processes.approve_verification(&root, &task_id, command)
}

// Three of these are Tauri-injected handles; the rest are the command's wire
// contract, so collapsing them into a struct would only move the arity.
#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub async fn coding_verification_run(
    app: AppHandle,
    access: State<'_, FilesystemAccess>,
    processes: State<'_, CodingProcesses>,
    root: String,
    task_id: String,
    kind: VerificationKind,
    command: String,
    timeout_secs: Option<u64>,
    requested_run_id: Option<String>,
    approval_token: Option<String>,
) -> Result<VerificationRecord, String> {
    let root = access.require_workspace(&root)?;
    run(
        app,
        &processes,
        root,
        task_id,
        kind,
        command,
        timeout_secs,
        requested_run_id,
        approval_token,
    )
    .await
}

#[tauri::command]
pub async fn coding_verification_cancel(
    processes: State<'_, CodingProcesses>,
    run_id: String,
) -> Result<(), String> {
    processes.cancel(&run_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[derive(Default)]
    struct CapturedEvents {
        chunks: Mutex<Vec<(String, String)>>,
        updates: Mutex<Vec<VerificationRecord>>,
    }

    #[cfg(unix)]
    impl VerificationEvents for CapturedEvents {
        fn output(
            &self,
            _root: &Path,
            _task_id: &str,
            run_id: &str,
            _stream: &'static str,
            line: &str,
        ) {
            self.chunks
                .lock()
                .unwrap()
                .push((run_id.to_string(), line.to_string()));
        }

        fn updated(&self, record: &VerificationRecord) {
            self.updates.lock().unwrap().push(record.clone());
        }
    }

    // Re-execute only this test in a child with its own data profile and store.
    // No process-global environment mutation can race the rest of the suite.
    #[cfg(unix)]
    fn isolated_runner_case(kind: &str, test_name: &str) {
        if std::env::var("ECHO_VERIFICATION_FIXTURE_KIND").as_deref() == Ok(kind) {
            let scope = PathBuf::from(std::env::var_os("ECHO_VERIFICATION_FIXTURE_ROOT").unwrap());
            let nonce = std::env::var("ECHO_VERIFICATION_FIXTURE_NONCE").unwrap();
            assert!(scope.starts_with(std::env::temp_dir().canonicalize().unwrap()));
            assert_eq!(std::fs::read_to_string(scope.join("nonce")).unwrap(), nonce);
            assert_eq!(crate::paths::echo_agent_home_dir(), scope.join("home"));
            assert_eq!(
                crate::paths::echo_agent_home_dir().canonicalize().unwrap(),
                scope.join("home")
            );
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .unwrap()
                .block_on(async {
                    tokio::time::timeout(
                        std::time::Duration::from_secs(20),
                        exercise_native_runner(scope.join("workspace 中文"), kind),
                    )
                    .await
                })
                .unwrap();
            std::fs::write(scope.join("proof-complete"), format!("{kind}:{nonce}")).unwrap();
            return;
        }

        let directory = tempfile::tempdir().unwrap();
        let scope = directory.path().canonicalize().unwrap();
        let home = scope.join("home");
        std::fs::create_dir(&home).unwrap();
        let nonce = uuid::Uuid::now_v7().to_string();
        std::fs::write(scope.join("nonce"), &nonce).unwrap();
        let stdout_path = scope.join("stdout");
        let stderr_path = scope.join("stderr");
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", test_name, "--nocapture", "--test-threads=1"])
            .current_dir(&scope)
            .env("ECHO_VERIFICATION_FIXTURE_KIND", kind)
            .env("ECHO_VERIFICATION_FIXTURE_ROOT", &scope)
            .env("ECHO_VERIFICATION_FIXTURE_NONCE", &nonce)
            .env("ECHO_AGENT_HOME", &home)
            .env("GROK_HOME", home.join("grok"))
            .stdin(Stdio::null())
            .stdout(std::fs::File::create(&stdout_path).unwrap())
            .stderr(std::fs::File::create(&stderr_path).unwrap());
        let mut child = crate::process_supervisor::spawn_sync(command).unwrap();
        let deadline = Instant::now() + std::time::Duration::from_secs(30);
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if Instant::now() >= deadline {
                crate::process_supervisor::stop_sync(&mut child);
                panic!("isolated verification subprocess exceeded its deadline");
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        assert!(
            status.success(),
            "isolated verification failed:\n{}\n{}",
            std::fs::read_to_string(stdout_path).unwrap(),
            std::fs::read_to_string(stderr_path).unwrap()
        );
        // A misspelled exact filter would otherwise exit successfully with zero tests.
        assert_eq!(
            std::fs::read_to_string(scope.join("proof-complete")).unwrap(),
            format!("{kind}:{nonce}")
        );
    }

    #[cfg(unix)]
    async fn exercise_native_runner(root: PathBuf, kind: &str) {
        std::fs::create_dir(&root).unwrap();
        std::fs::write(
            root.join("verification-fixture.sh"),
            r#"child=''
trap 'kill "$child" 2>/dev/null; wait "$child"; exit 0' TERM
sleep 30 &
child=$!
printf '%s\n' "$child" > descendant.pid
printf '%s\n' "$$" > shell.pid
printf 'READY STDOUT\n'
printf 'READY STDERR\n' >&2
wait "$child"
printf 'LATE STDOUT\n'
printf 'LATE STDERR\n' >&2
printf late > late-write
"#,
        )
        .unwrap();
        let coding_task =
            task::create_task(&root, "subprocess", "synthetic local verification").unwrap();
        crate::coding::changeset::capture_filesystem_baseline(&root, &coding_task.id).unwrap();
        assert!(store::workspace_dir(&root).starts_with(crate::paths::echo_agent_home_dir()));
        let processes = Arc::new(CodingProcesses::new());
        let events = Arc::new(CapturedEvents::default());
        let command = "exec /bin/sh ./verification-fixture.sh".to_string();
        let run_id = format!("actual-{kind}");
        assert!(high_risk_command_reason(&command, &root).is_none());
        assert!(run_with_events(
            events.clone(),
            &processes,
            root.clone(),
            coding_task.id.clone(),
            VerificationKind::Custom,
            command.clone(),
            Some(1),
            Some(run_id.clone()),
            None,
        )
        .await
        .unwrap_err()
        .contains("需要用户确认"));
        assert!(!root.join("shell.pid").exists());
        assert!(list_records(&root, &coding_task.id).is_empty());

        let approval = processes
            .approve_verification(&root, &coding_task.id, &command)
            .unwrap();
        let execution = {
            let (events, processes, root, task_id, command, run_id, approval) = (
                events.clone(),
                processes.clone(),
                root.clone(),
                coding_task.id.clone(),
                command.clone(),
                run_id.clone(),
                approval.clone(),
            );
            tokio::spawn(async move {
                run_with_events(
                    events,
                    &processes,
                    root,
                    task_id,
                    VerificationKind::Custom,
                    command,
                    Some(1), // The runner honors the one-second timeout accepted by project settings.
                    Some(run_id),
                    Some(approval),
                )
                .await
            })
        };
        let pid_files = ["shell.pid", "descendant.pid"];
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let chunks = events.chunks.lock().unwrap().clone();
                let ready = ["READY STDOUT", "READY STDERR"]
                    .iter()
                    .all(|expected| chunks.iter().any(|(_, line)| line.as_str() == *expected));
                if ready
                    && pid_files.iter().all(|file| {
                        std::fs::read_to_string(root.join(file))
                            .ok()
                            .is_some_and(|text| text.trim().parse::<i32>().is_ok())
                    })
                {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let pids: Vec<i32> = pid_files
            .iter()
            .map(|file| {
                std::fs::read_to_string(root.join(file))
                    .unwrap()
                    .trim()
                    .parse()
                    .unwrap()
            })
            .collect();
        assert_ne!(pids[0], pids[1]);
        for pid in &pids {
            assert!(*pid > 1 && *pid != std::process::id() as i32);
            assert_eq!(
                unsafe { libc::kill(*pid, 0) },
                0,
                "fixture process must really be running"
            );
        }
        assert!(processes
            .register(&run_id, CancellationToken::new())
            .is_err());
        if kind == "cancel" {
            processes.cancel(&run_id).unwrap();
        }
        let record = execution.await.unwrap().unwrap();
        assert_eq!(
            record.status,
            if kind == "cancel" {
                VerificationStatus::Cancelled
            } else {
                VerificationStatus::TimedOut
            }
        );
        // The graceful fixture exits zero: cancellation/timeout still cannot become Passed.
        assert_eq!(record.exit_code, Some(0));
        if kind == "timeout" {
            assert!(record.duration_ms >= 1_000);
        }
        assert!(record.stdout.contains("READY STDOUT"));
        assert!(record.stderr.contains("READY STDERR"));
        assert!(!record.stdout.contains("LATE"));
        assert!(!record.stderr.contains("LATE"));
        assert!(
            processes.cancel(&run_id).is_err(),
            "completed run must leave the registry"
        );
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while pids.iter().any(|pid| unsafe { libc::kill(*pid, 0) } == 0) {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        for pid in &pids {
            assert_eq!(unsafe { libc::kill(*pid, 0) }, -1);
            assert_eq!(
                std::io::Error::last_os_error().raw_os_error(),
                Some(libc::ESRCH)
            );
        }
        assert!(!root.join("late-write").exists());
        let records = list_records(&root, &coding_task.id);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].id, record.id);
        assert_eq!(records[0].status, record.status);
        assert!(record.content_revision.is_some());

        if kind == "cancel" {
            assert!(run_with_events(
                events.clone(),
                &processes,
                root.clone(),
                coding_task.id.clone(),
                VerificationKind::Custom,
                command,
                Some(5),
                Some(run_id.clone()),
                Some(approval),
            )
            .await
            .unwrap_err()
            .contains("尚未确认或授权已过期"));
            let retry_command =
                "printf 'RETRY STDOUT\\n'; printf 'RETRY STDERR\\n' >&2".to_string();
            let retry_approval = processes
                .approve_verification(&root, &coding_task.id, &retry_command)
                .unwrap();
            let retry = run_with_events(
                events.clone(),
                &processes,
                root.clone(),
                coding_task.id.clone(),
                VerificationKind::Custom,
                retry_command,
                Some(5),
                Some(run_id.clone()),
                Some(retry_approval),
            )
            .await
            .unwrap();
            assert_eq!(retry.status, VerificationStatus::Passed);
            assert_eq!(retry.exit_code, Some(0));
            assert_ne!(retry.id, record.id);
            assert!(retry.stdout.contains("RETRY STDOUT"));
            assert!(retry.stderr.contains("RETRY STDERR"));
            assert!(processes.cancel(&run_id).is_err());
            let records = list_records(&root, &coding_task.id);
            assert_eq!(records.len(), 2);
            assert_eq!(records[0].id, record.id);
            assert_eq!(records[0].status, VerificationStatus::Cancelled);
            assert_eq!(records[1].id, retry.id);
            assert_eq!(records[1].status, VerificationStatus::Passed);
            assert_eq!(events.updates.lock().unwrap().len(), 2);
        } else {
            assert_eq!(events.updates.lock().unwrap().len(), 1);
        }
        assert!(!events
            .chunks
            .lock()
            .unwrap()
            .iter()
            .any(|(_, line)| line.contains("LATE")));
        assert!(!root.join("late-write").exists());
    }

    #[cfg(unix)]
    #[test]
    fn native_subprocess_cancel_reaps_tree_and_same_id_retry_requires_new_approval() {
        isolated_runner_case(
            "cancel",
            "coding::verification::tests::native_subprocess_cancel_reaps_tree_and_same_id_retry_requires_new_approval",
        );
    }

    #[cfg(unix)]
    #[test]
    fn native_subprocess_timeout_is_not_zero_exit_success_and_reaps_tree() {
        isolated_runner_case(
            "timeout",
            "coding::verification::tests::native_subprocess_timeout_is_not_zero_exit_success_and_reaps_tree",
        );
    }

    fn temp_root() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("coding-verify-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn missing_tools_are_environment_errors_not_code_failures() {
        let unavailable = record_from_parts(
            "task",
            VerificationKind::Test,
            "python3 -m pytest",
            Some(1),
            String::new(),
            "No module named pytest".into(),
            1,
            false,
            false,
        );
        assert_eq!(
            unavailable.status,
            VerificationStatus::EnvironmentUnavailable
        );
        let failed = record_from_parts(
            "task",
            VerificationKind::Test,
            "python3 -m pytest",
            Some(1),
            "1 failed".into(),
            String::new(),
            1,
            false,
            false,
        );
        assert_eq!(failed.status, VerificationStatus::Failed);
        let cancelled = record_from_parts(
            "task",
            VerificationKind::Test,
            "missing",
            Some(127),
            String::new(),
            String::new(),
            1,
            false,
            true,
        );
        assert_eq!(cancelled.status, VerificationStatus::Cancelled);
    }

    #[test]
    fn python_detection_requires_declared_tools_and_uses_environment() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("pyproject.toml"),
            "[project]\nname='demo'\n",
        )
        .unwrap();
        assert!(detect_commands(dir.path()).is_empty());
        std::fs::write(
            dir.path().join("pyproject.toml"),
            "[dependency-groups]\ndev=['pytest>=8']\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("uv.lock"), "").unwrap();
        let commands = detect_commands(dir.path());
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].command, "uv run --no-sync python -m pytest");
    }

    #[test]
    fn windows_selects_native_java_wrappers() {
        let dir = tempfile::tempdir().unwrap();
        for file in [
            "pom.xml",
            "mvnw",
            "mvnw.cmd",
            "build.gradle",
            "gradlew",
            "gradlew.bat",
        ] {
            std::fs::write(dir.path().join(file), "").unwrap();
        }
        let commands = detect_commands_for_platform(dir.path(), true);
        assert!(commands.iter().any(|c| c.command == r".\mvnw.cmd -B test"));
        assert!(commands.iter().any(|c| c.command == r".\gradlew.bat test"));
    }

    #[tokio::test]
    async fn output_without_newlines_is_bounded_and_preserves_utf8() {
        let input = "记".repeat(100_000);
        let mut chunks = Vec::new();
        read_output(input.as_bytes(), |chunk| chunks.push(chunk)).await;
        assert!(chunks.iter().all(|chunk| chunk.len() <= 8192));
        assert_eq!(chunks.concat(), input);
    }

    #[test]
    fn detects_npm_scripts_by_kind() {
        let root = temp_root();
        std::fs::write(
            root.join("package.json"),
            r#"{"scripts":{"build":"tsc --noEmit && vite build","lint":"eslint .","test":"vitest run"}}"#,
        )
        .unwrap();
        let detected = detect_commands(&root);
        assert!(detected
            .iter()
            .any(|entry| entry.kind == VerificationKind::Build && entry.command.contains("build")));
        assert!(detected
            .iter()
            .any(|entry| entry.kind == VerificationKind::Lint && entry.command.contains("lint")));
        assert!(detected
            .iter()
            .any(|entry| entry.kind == VerificationKind::Test && entry.command.contains("test")));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn detects_verification_across_pnpm_workspace_packages() {
        let root = temp_root();
        std::fs::write(
            root.join("package.json"),
            r#"{"private":true,"workspaces":["packages/*"]}"#,
        )
        .unwrap();
        std::fs::write(
            root.join("pnpm-workspace.yaml"),
            "packages:\n  - packages/*\n",
        )
        .unwrap();
        let package = root.join("packages/api");
        std::fs::create_dir_all(&package).unwrap();
        std::fs::write(
            package.join("package.json"),
            r#"{"scripts":{"build":"tsc","test":"vitest run"}}"#,
        )
        .unwrap();
        let detected = detect_commands(&root);
        assert!(detected
            .iter()
            .any(|entry| entry.command == "pnpm -r --if-present run build"));
        assert!(detected
            .iter()
            .any(|entry| entry.command == "pnpm -r --if-present run test"));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_plan_declared_command_requires_consent_even_when_manifest_detected() {
        let root = temp_root();
        std::fs::write(
            root.join("package.json"),
            r#"{"scripts":{"test":"vitest run"}}"#,
        )
        .unwrap();
        let mut coding_task = task::create_task(&root, "verify", "run tests").unwrap();
        coding_task.task_nodes[0].verification_commands = vec!["npm run test".into()];
        assert!(is_manifest_verification(&root, "npm run test"));
        assert!(is_planned_verification(&coding_task, "npm run test"));
        std::fs::remove_dir_all(store::workspace_dir(&root)).ok();
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn detects_cargo_and_maven_projects() {
        let root = temp_root();
        std::fs::write(root.join("Cargo.toml"), "[package]\nname = \"demo\"\n").unwrap();
        let detected = detect_commands(&root);
        assert!(detected.iter().any(|entry| entry.command == "cargo build"));
        assert!(detected.iter().any(|entry| entry.command == "cargo test"));
        std::fs::remove_dir_all(&root).ok();

        let maven = temp_root();
        std::fs::write(maven.join("pom.xml"), "<project></project>").unwrap();
        let detected = detect_commands(&maven);
        assert!(detected
            .iter()
            .any(|entry| entry.command.starts_with("mvn")));
        std::fs::remove_dir_all(&maven).ok();
    }

    #[test]
    fn parses_vitest_summary() {
        let summary =
            parse_test_output("Test Files  2 passed (2)\n Tests  42 passed | 1 skipped (43)")
                .expect("vitest output should parse");
        assert_eq!(summary.passed, 42);
        assert_eq!(summary.skipped, 1);
        assert_eq!(summary.failed, 0);
    }

    #[test]
    fn parses_pytest_and_junit_and_cargo_summaries() {
        let pytest = parse_test_output("=== 3 failed, 12 passed, 2 skipped in 4.21s ===").unwrap();
        assert_eq!(pytest.failed, 3);
        assert_eq!(pytest.passed, 12);
        assert_eq!(pytest.skipped, 2);

        let junit = parse_test_output("Tests run: 24, Failures: 2, Errors: 1, Skipped: 3").unwrap();
        assert_eq!(junit.total, 24);
        assert_eq!(junit.failed, 3);
        assert_eq!(junit.skipped, 3);

        let cargo =
            parse_test_output("test result: FAILED. 8 passed; 2 failed; 1 ignored").unwrap();
        assert_eq!(cargo.passed, 8);
        assert_eq!(cargo.failed, 2);
        assert_eq!(cargo.skipped, 1);
    }

    #[test]
    fn vitest_file_tally_is_not_mistaken_for_case_tally() {
        // "Test Files 2 passed" appears before "Tests 42 passed"; reading the
        // first match would report 2 cases instead of 42.
        let summary = parse_test_output(
            "\n Test Files  2 passed (2)\n      Tests  42 passed (42)\n   Duration  1.20s\n",
        )
        .unwrap();
        assert_eq!(summary.passed, 42);
        assert_eq!(summary.total, 42);
    }

    #[test]
    fn unparseable_output_yields_none() {
        assert!(parse_test_output("Compiling demo v0.1.0\nFinished in 3s").is_none());
    }

    #[test]
    fn status_follows_exit_code_not_output_text() {
        // Output that reads like success must not override a non-zero exit code.
        let record = record_from_parts(
            "task-1",
            VerificationKind::Test,
            "pnpm test",
            Some(1),
            "All tests passed!".into(),
            String::new(),
            120,
            false,
            false,
        );
        assert_eq!(record.status, VerificationStatus::Failed);

        let passing = record_from_parts(
            "task-1",
            VerificationKind::Build,
            "pnpm build",
            Some(0),
            String::new(),
            "warning: unused import".into(),
            80,
            false,
            false,
        );
        assert_eq!(passing.status, VerificationStatus::Passed);
    }

    #[test]
    fn timeout_and_cancel_are_distinct_from_failure() {
        let timed_out = record_from_parts(
            "task-1",
            VerificationKind::Test,
            "pnpm test",
            None,
            String::new(),
            String::new(),
            300_000,
            true,
            false,
        );
        assert_eq!(timed_out.status, VerificationStatus::TimedOut);

        let cancelled = record_from_parts(
            "task-1",
            VerificationKind::Test,
            "pnpm test",
            None,
            String::new(),
            String::new(),
            500,
            false,
            true,
        );
        assert_eq!(cancelled.status, VerificationStatus::Cancelled);
    }

    #[test]
    fn retained_output_is_capped_while_keeping_the_tail() {
        // A verbose build must be drained without letting memory grow with it,
        // and the tail matters more than the head for diagnosing a failure.
        let mut buffer = String::new();
        let mut dropped = false;
        for index in 0..20_000 {
            push_bounded(
                &mut buffer,
                &format!("line {index} {}", "x".repeat(80)),
                &mut dropped,
            );
        }
        assert!(dropped);
        assert!(buffer.len() <= MAX_OUTPUT_BYTES);
        assert!(buffer.contains("line 19999"));
        assert!(!buffer.contains("line 0 "));
        assert!(label_dropped(buffer, dropped).starts_with("…较早输出已省略…"));
    }

    #[test]
    fn short_output_is_not_labelled_as_dropped() {
        let mut buffer = String::new();
        let mut dropped = false;
        push_bounded(&mut buffer, "hello", &mut dropped);
        assert!(!dropped);
        assert_eq!(label_dropped(buffer, dropped), "hello\n");
    }

    #[test]
    fn records_append_and_reload_in_order() {
        let root = temp_root();
        for index in 0..3 {
            let record = record_from_parts(
                "task-1",
                VerificationKind::Test,
                &format!("cmd-{index}"),
                Some(0),
                String::new(),
                String::new(),
                10,
                false,
                false,
            );
            append_record(&root, &record).unwrap();
        }
        let records = list_records(&root, "task-1");
        assert_eq!(records.len(), 3);
        assert_eq!(records[0].command, "cmd-0");
        assert_eq!(records[2].command, "cmd-2");
        clear_records(&root, "task-1").unwrap();
        assert_eq!(
            list_records(&root, "task-1").len(),
            3,
            "starting another verification must retain all historical command outcomes"
        );
        std::fs::remove_dir_all(&root).ok();
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn independent_checks_keep_same_revision_results_and_use_project_execution_config() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        std::fs::create_dir(root.join("checks")).unwrap();
        std::fs::write(root.join("checks/marker.txt"), "marker\n").unwrap();
        let mut coding_task =
            task::create_task(&root, "Independent checks", "adjust behavior").unwrap();
        coding_task.phase = task::TaskPhase::Delivered;
        task::save(&root, &coding_task).unwrap();
        crate::coding::changeset::capture_filesystem_baseline(&root, &coding_task.id).unwrap();
        std::fs::write(root.join("source.txt"), "actual edit\n").unwrap();
        crate::coding::changeset::sync_changes(&root, &coding_task.id)
            .await
            .unwrap();
        let config = VerificationProjectConfig {
            checks: vec![
                ConfiguredCheck {
                    id: "file".into(),
                    kind: VerificationKind::Test,
                    command: "test -f marker.txt".into(),
                    label: "cwd test".into(),
                    cwd: "checks".into(),
                    env: BTreeMap::new(),
                    timeout_secs: Some(15),
                    auto_run: false,
                },
                ConfiguredCheck {
                    id: "environment".into(),
                    kind: VerificationKind::Build,
                    command: "test \"$ECHO_CHECK_LABEL\" = alpha".into(),
                    label: "env check".into(),
                    cwd: ".".into(),
                    env: BTreeMap::from([("ECHO_CHECK_LABEL".into(), "alpha".into())]),
                    timeout_secs: Some(15),
                    auto_run: false,
                },
            ],
        };
        store::write_json(
            &store::workspace_dir(&root).join("verification-config.json"),
            &config,
        )
        .unwrap();
        let processes = CodingProcesses::default();
        let events = Arc::new(CapturedEvents::default());
        for (index, check) in config.checks.iter().enumerate() {
            crate::coding::orchestrator::apply(
                &root,
                &coding_task.id,
                crate::coding::orchestrator::OrchestratorEvent::VerificationStarted,
            )
            .unwrap();
            let record = run_with_events(
                events.clone(),
                &processes,
                root.clone(),
                coding_task.id.clone(),
                check.kind,
                check.command.clone(),
                None,
                Some(format!("independent-{index}")),
                None,
            )
            .await
            .unwrap();
            assert_eq!(
                record.status,
                VerificationStatus::Passed,
                "{}",
                record.stderr
            );
            assert_eq!(
                record.cwd.as_deref(),
                Some(
                    root.join(&check.cwd)
                        .canonicalize()
                        .unwrap()
                        .to_string_lossy()
                        .as_ref()
                )
            );
            let (finished, _) = crate::coding::orchestrator::apply(
                &root,
                &coding_task.id,
                crate::coding::orchestrator::OrchestratorEvent::VerificationFinished,
            )
            .unwrap();
            assert_eq!(finished.phase, task::TaskPhase::Delivered);
            assert!(finished.blocker.is_none());
            assert_eq!(list_records(&root, &coding_task.id).len(), index + 1);
        }
        let set = crate::coding::changeset::load(&root, &coding_task.id);
        assert_eq!(current_records(&root, &coding_task.id, &set).len(), 2);
        assert!(set.verification_is_current());
        let mut changed_config = config;
        changed_config.checks[1]
            .env
            .insert("ECHO_CHECK_LABEL".into(), "beta".into());
        store::write_json(
            &store::workspace_dir(&root).join("verification-config.json"),
            &changed_config,
        )
        .unwrap();
        assert!(current_records(&root, &coding_task.id, &set).is_empty());
        assert_eq!(list_records(&root, &coding_task.id).len(), 2);
    }
}
