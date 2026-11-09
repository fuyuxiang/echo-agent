//! Diagnostics centre: turn raw compiler / linter / test output into
//! structured problems that carry a file, a line and a stable fingerprint.
//!
//! The fingerprint is what lets the repair engine tell "the same failure came
//! back" apart from "my fix introduced something new", so it deliberately
//! ignores volatile parts of a message such as timings and temp directories.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tauri::State;

use crate::coding::store;
use crate::coding::verification::{VerificationKind, VerificationRecord, VerificationStatus};
use crate::shell_fs::FilesystemAccess;

const MAX_PROBLEMS_PER_RECORD: usize = 200;

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum ProblemKind {
    Compile,
    Syntax,
    Type,
    Lint,
    TestFailure,
    Runtime,
    Dependency,
    Configuration,
    Documentation,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum ProblemSeverity {
    Error,
    Warning,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Problem {
    pub id: String,
    pub kind: ProblemKind,
    pub severity: ProblemSeverity,
    pub message: String,
    pub file: Option<String>,
    pub line: Option<u32>,
    pub column: Option<u32>,
    /// Failing test name, or the symbol a compiler error points at.
    pub symbol: Option<String>,
    pub source_command: String,
    /// Stable identity across repair rounds; see module docs.
    pub fingerprint: String,
}

fn diagnostics_path(root: &Path, task_id: &str) -> PathBuf {
    store::task_dir(root, task_id).join("diagnostics.jsonl")
}

/// Strip volatile substrings so the same underlying failure keeps one identity.
fn normalize_for_fingerprint(message: &str) -> String {
    let without_temp = regex::Regex::new(r"(?:/tmp|/var/folders|[A-Za-z]:\\Temp)[^\s:]*")
        .map(|expression| expression.replace_all(message, "<tmp>").into_owned())
        .unwrap_or_else(|_| message.to_string());
    let without_durations = regex::Regex::new(r"\b\d+(?:\.\d+)?\s*(?:ms|s|sec|seconds)\b")
        .map(|expression| {
            expression
                .replace_all(&without_temp, "<duration>")
                .into_owned()
        })
        .unwrap_or(without_temp);
    let without_hex = regex::Regex::new(r"\b0x[0-9a-fA-F]+\b")
        .map(|expression| {
            expression
                .replace_all(&without_durations, "<addr>")
                .into_owned()
        })
        .unwrap_or(without_durations);
    without_hex.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn fingerprint_of(
    kind: ProblemKind,
    file: Option<&str>,
    line: Option<u32>,
    message: &str,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(format!("{kind:?}").as_bytes());
    hasher.update(file.unwrap_or("").as_bytes());
    hasher.update(line.unwrap_or(0).to_le_bytes());
    hasher.update(normalize_for_fingerprint(message).as_bytes());
    hasher
        .finalize()
        .iter()
        .take(8)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Classify a message when the surrounding parser did not already know its
/// kind. Dependency and configuration checks come first because their messages
/// often also contain the word "error".
pub fn classify(message: &str) -> ProblemKind {
    let lowered = message.to_lowercase();
    let dependency = [
        "npm err!",
        "could not resolve dependency",
        "no matching distribution",
        "could not find artifact",
        "unresolved dependency",
        "pip install",
        "no matching package",
    ];
    if dependency.iter().any(|needle| lowered.contains(needle)) {
        return ProblemKind::Dependency;
    }
    let configuration = [
        "tsconfig",
        "eslintrc",
        ".env",
        "application.yml",
        "application.properties",
        "failed to parse",
        "invalid configuration",
    ];
    if configuration.iter().any(|needle| lowered.contains(needle)) {
        return ProblemKind::Configuration;
    }
    if lowered.contains("syntaxerror") || lowered.contains("parse error") {
        return ProblemKind::Syntax;
    }
    if lowered.contains("type") && (lowered.contains("error") || lowered.contains("mismatch")) {
        return ProblemKind::Type;
    }
    if lowered.contains("assertionerror")
        || lowered.contains("test failed")
        || lowered.starts_with("failed ")
    {
        return ProblemKind::TestFailure;
    }
    if lowered.contains("exception") || lowered.contains("stack trace") {
        return ProblemKind::Runtime;
    }
    ProblemKind::Compile
}

fn severity_of(message: &str) -> ProblemSeverity {
    if message.to_lowercase().contains("warning") {
        ProblemSeverity::Warning
    } else {
        ProblemSeverity::Error
    }
}

fn relative_path(raw: &str) -> String {
    let normalized = raw.replace('\\', "/");
    let trimmed = normalized.trim_start_matches("./");
    trimmed
        .rsplit_once("/repo/")
        .map(|(_, tail)| tail.to_string())
        .unwrap_or_else(|| trimmed.to_string())
}

fn build(
    kind: ProblemKind,
    message: String,
    file: Option<String>,
    line: Option<u32>,
    column: Option<u32>,
    symbol: Option<String>,
    command: &str,
) -> Problem {
    let fingerprint = fingerprint_of(kind, file.as_deref(), line, &message);
    Problem {
        id: uuid::Uuid::now_v7().to_string(),
        kind,
        severity: severity_of(&message),
        message,
        file,
        line,
        column,
        symbol,
        source_command: command.to_string(),
        fingerprint,
    }
}

pub fn documentation_problem(file: Option<String>, message: String) -> Problem {
    build(
        ProblemKind::Documentation,
        message,
        file,
        None,
        None,
        None,
        "documentation-safety",
    )
}

/// Parse one verification record into problems. A passing record yields none,
/// so text that merely looks like an error can never manufacture a problem.
pub fn parse_record(record: &VerificationRecord) -> Vec<Problem> {
    if matches!(
        record.status,
        VerificationStatus::Passed | VerificationStatus::EvidenceInvalidated
    ) {
        return Vec::new();
    }
    let combined = format!("{}\n{}", record.stdout, record.stderr);
    let lines: Vec<&str> = combined.lines().collect();
    let mut problems: Vec<Problem> = Vec::new();
    let mut eslint_file: Option<String> = None;

    let tsc =
        regex::Regex::new(r"^(?P<file>[^\s(]+)\((?P<line>\d+),(?P<col>\d+)\):\s*(?P<body>.+)$")
            .expect("valid tsc pattern");
    let unix_style = regex::Regex::new(
        r"^(?P<file>[^\s:]+\.[A-Za-z]+):(?P<line>\d+)(?::(?P<col>\d+))?:\s*(?P<body>.+)$",
    )
    .expect("valid unix pattern");
    let eslint_head = regex::Regex::new(r"^(?P<file>(?:/|\./|[A-Za-z]:\\)[^\s]+\.[A-Za-z]+)\s*$")
        .expect("valid eslint head pattern");
    let eslint_row = regex::Regex::new(
        r"^\s+(?P<line>\d+):(?P<col>\d+)\s+(?P<severity>error|warning)\s+(?P<body>.+)$",
    )
    .expect("valid eslint row pattern");
    let rust_head = regex::Regex::new(r"^(?:error|warning)(?:\[[^\]]+\])?:\s*(?P<body>.+)$")
        .expect("valid rustc head pattern");
    let rust_location =
        regex::Regex::new(r"^\s*-->\s*(?P<file>[^\s:]+):(?P<line>\d+):(?P<col>\d+)")
            .expect("valid rustc location pattern");
    let pytest_failure = regex::Regex::new(
        r"^FAILED\s+(?P<file>[^\s:]+)::(?P<symbol>[^\s]+)(?:\s+-\s+(?P<body>.+))?$",
    )
    .expect("valid pytest pattern");

    let mut index = 0;
    while index < lines.len() && problems.len() < MAX_PROBLEMS_PER_RECORD {
        let line = lines[index];
        index += 1;
        let trimmed = line.trim_end();
        if trimmed.trim().is_empty() {
            continue;
        }

        if let Some(captures) = eslint_head.captures(trimmed) {
            eslint_file = Some(relative_path(&captures["file"]));
            continue;
        }
        if let Some(captures) = eslint_row.captures(trimmed) {
            let body = captures["body"].trim().to_string();
            let message = format!("{} {}", &captures["severity"], body);
            problems.push(build(
                ProblemKind::Lint,
                message,
                eslint_file.clone(),
                captures["line"].parse().ok(),
                captures["col"].parse().ok(),
                None,
                &record.command,
            ));
            continue;
        }
        if let Some(captures) = pytest_failure.captures(trimmed) {
            let body = captures
                .name("body")
                .map(|value| value.as_str().to_string())
                .unwrap_or_else(|| trimmed.to_string());
            problems.push(build(
                ProblemKind::TestFailure,
                body,
                Some(relative_path(&captures["file"])),
                None,
                None,
                Some(captures["symbol"].to_string()),
                &record.command,
            ));
            continue;
        }
        if let Some(captures) = tsc.captures(trimmed) {
            let body = captures["body"].to_string();
            let kind = if record.kind == VerificationKind::Lint {
                ProblemKind::Lint
            } else {
                classify(&body)
            };
            problems.push(build(
                kind,
                body,
                Some(relative_path(&captures["file"])),
                captures["line"].parse().ok(),
                captures["col"].parse().ok(),
                None,
                &record.command,
            ));
            continue;
        }
        if let Some(captures) = rust_head.captures(trimmed) {
            let body = captures["body"].to_string();
            // rustc prints the location on the next line; consume it when present.
            let (file, line_number, column) = lines
                .get(index)
                .and_then(|next| rust_location.captures(next))
                .map(|location| {
                    (
                        Some(relative_path(&location["file"])),
                        location["line"].parse().ok(),
                        location["col"].parse().ok(),
                    )
                })
                .unwrap_or((None, None, None));
            if file.is_some() {
                index += 1;
            }
            let kind = if trimmed.starts_with("warning") && record.kind == VerificationKind::Lint {
                ProblemKind::Lint
            } else {
                classify(&body)
            };
            problems.push(build(
                kind,
                body,
                file,
                line_number,
                column,
                None,
                &record.command,
            ));
            continue;
        }
        if let Some(captures) = unix_style.captures(trimmed) {
            let body = captures["body"].to_string();
            let kind = match record.kind {
                VerificationKind::TypeCheck => ProblemKind::Type,
                VerificationKind::Lint => ProblemKind::Lint,
                _ => classify(&body),
            };
            problems.push(build(
                kind,
                body,
                Some(relative_path(&captures["file"])),
                captures["line"].parse().ok(),
                captures
                    .name("col")
                    .and_then(|value| value.as_str().parse().ok()),
                None,
                &record.command,
            ));
            continue;
        }
    }

    // A failing run with no recognisable location still deserves one problem so
    // the repair engine has something to work from.
    if problems.is_empty() {
        let visible: Vec<&str> = combined
            .lines()
            .filter(|line| !line.trim().is_empty())
            .collect();
        let tail = visible
            .iter()
            .rev()
            .take(3)
            .rev()
            .copied()
            .collect::<Vec<_>>()
            .join("\n");
        let message = if tail.is_empty() {
            format!("{} 执行失败，未产生可解析输出", record.command)
        } else {
            tail
        };
        let kind = classify(&message);
        problems.push(build(
            kind,
            message,
            None,
            None,
            None,
            None,
            &record.command,
        ));
    }
    problems
}

pub fn save_snapshot(root: &Path, task_id: &str, problems: &[Problem]) -> Result<(), String> {
    let path = diagnostics_path(root, task_id);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| format!("无法创建目录：{error}"))?;
    }
    let body = problems
        .iter()
        .map(|problem| serde_json::to_string(problem).unwrap_or_default())
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(
        &path,
        if body.is_empty() {
            String::new()
        } else {
            format!("{body}\n")
        },
    )
    .map_err(|error| format!("写入诊断快照失败：{error}"))
}

pub fn load_snapshot(root: &Path, task_id: &str) -> Vec<Problem> {
    store::read_jsonl(&diagnostics_path(root, task_id))
}

#[tauri::command]
pub async fn coding_diagnostics_list(
    access: State<'_, FilesystemAccess>,
    root: String,
    task_id: String,
) -> Result<Vec<Problem>, String> {
    let root = access.require_workspace(&root)?;
    tokio::task::spawn_blocking(move || load_snapshot(&root, &task_id))
        .await
        .map_err(|error| format!("读取诊断结果失败：{error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coding::verification::record_from_parts;

    fn record(kind: VerificationKind, stdout: &str, stderr: &str) -> VerificationRecord {
        record_from_parts(
            "task-1",
            kind,
            "cmd",
            Some(1),
            stdout.to_string(),
            stderr.to_string(),
            100,
            false,
            false,
        )
    }

    #[test]
    fn parses_typescript_errors_with_position() {
        let output = "src/auth.ts(42,17): error TS2345: Argument of type 'string' is not assignable to parameter of type 'number'.";
        let problems = parse_record(&record(VerificationKind::TypeCheck, output, ""));
        assert_eq!(problems.len(), 1);
        assert_eq!(problems[0].kind, ProblemKind::Type);
        assert_eq!(problems[0].file.as_deref(), Some("src/auth.ts"));
        assert_eq!(problems[0].line, Some(42));
        assert_eq!(problems[0].column, Some(17));
        assert!(problems[0].message.contains("TS2345"));
    }

    #[test]
    fn parses_eslint_stylish_block() {
        let output = "/repo/src/app.ts\n  12:5  error  'foo' is assigned a value but never used  no-unused-vars\n  20:1  warning  Missing semicolon  semi\n";
        let problems = parse_record(&record(VerificationKind::Lint, output, ""));
        assert_eq!(problems.len(), 2);
        assert_eq!(problems[0].kind, ProblemKind::Lint);
        assert_eq!(problems[0].severity, ProblemSeverity::Error);
        assert_eq!(problems[0].line, Some(12));
        assert_eq!(problems[1].severity, ProblemSeverity::Warning);
        assert_eq!(problems[1].line, Some(20));
    }

    #[test]
    fn parses_rustc_error_with_following_location_line() {
        let output =
            "error[E0308]: mismatched types\n  --> src/main.rs:10:22\n   |\n10 |     let x: u32 = \"a\";\n";
        let problems = parse_record(&record(VerificationKind::Build, "", output));
        assert_eq!(problems.len(), 1);
        assert_eq!(problems[0].file.as_deref(), Some("src/main.rs"));
        assert_eq!(problems[0].line, Some(10));
        assert_eq!(problems[0].column, Some(22));
    }

    #[test]
    fn parses_pytest_failure_with_test_symbol() {
        let output =
            "FAILED tests/test_auth.py::test_login_rejects_expired - AssertionError: expected 401";
        let problems = parse_record(&record(VerificationKind::Test, output, ""));
        assert_eq!(problems.len(), 1);
        assert_eq!(problems[0].kind, ProblemKind::TestFailure);
        assert_eq!(problems[0].file.as_deref(), Some("tests/test_auth.py"));
        assert_eq!(
            problems[0].symbol.as_deref(),
            Some("test_login_rejects_expired")
        );
    }

    #[test]
    fn parses_javac_and_mypy_and_go() {
        let javac = parse_record(&record(
            VerificationKind::Build,
            "/repo/src/Main.java:15: error: cannot find symbol",
            "",
        ));
        assert_eq!(javac[0].line, Some(15));

        let mypy = parse_record(&record(
            VerificationKind::TypeCheck,
            "app/models.py:8: error: Incompatible return value type",
            "",
        ));
        assert_eq!(mypy[0].kind, ProblemKind::Type);
        assert_eq!(mypy[0].line, Some(8));

        let go = parse_record(&record(
            VerificationKind::Build,
            "./handler.go:31:5: undefined: parseToken",
            "",
        ));
        assert_eq!(go[0].kind, ProblemKind::Compile);
        assert_eq!(go[0].line, Some(31));
    }

    #[test]
    fn classifies_dependency_and_configuration_errors() {
        let dependency = parse_record(&record(
            VerificationKind::Build,
            "npm ERR! 404 Not Found - GET https://registry.npmjs.org/no-such-pkg",
            "",
        ));
        assert_eq!(dependency[0].kind, ProblemKind::Dependency);

        let configuration = parse_record(&record(
            VerificationKind::Build,
            "error: failed to parse tsconfig.json: Unexpected token }",
            "",
        ));
        assert_eq!(configuration[0].kind, ProblemKind::Configuration);
    }

    #[test]
    fn passing_record_yields_no_problems() {
        let passing = record_from_parts(
            "task-1",
            VerificationKind::Test,
            "pnpm test",
            Some(0),
            "error TS0000: this text must be ignored".into(),
            String::new(),
            10,
            false,
            false,
        );
        assert!(parse_record(&passing).is_empty());
    }

    #[test]
    fn fingerprint_is_stable_across_runs_but_differs_per_problem() {
        let first = parse_record(&record(
            VerificationKind::TypeCheck,
            "src/a.ts(1,1): error TS1: bad",
            "",
        ));
        let again = parse_record(&record(
            VerificationKind::TypeCheck,
            "src/a.ts(1,1): error TS1: bad",
            "",
        ));
        let other = parse_record(&record(
            VerificationKind::TypeCheck,
            "src/b.ts(1,1): error TS1: bad",
            "",
        ));
        assert_eq!(first[0].fingerprint, again[0].fingerprint);
        assert_ne!(first[0].fingerprint, other[0].fingerprint);
    }

    #[test]
    fn fingerprint_ignores_volatile_numbers_and_temp_paths() {
        let first = parse_record(&record(
            VerificationKind::Test,
            "FAILED tests/t.py::test_x - took 1234ms at /tmp/pytest-of-a/run-1/x",
            "",
        ));
        let second = parse_record(&record(
            VerificationKind::Test,
            "FAILED tests/t.py::test_x - took 9876ms at /tmp/pytest-of-a/run-9/x",
            "",
        ));
        assert_eq!(first[0].fingerprint, second[0].fingerprint);
    }

    #[test]
    fn snapshot_roundtrip() {
        let root = std::env::temp_dir().join(format!("coding-diag-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&root).unwrap();
        let problems = parse_record(&record(
            VerificationKind::TypeCheck,
            "src/a.ts(3,4): error TS9: nope",
            "",
        ));
        save_snapshot(&root, "task-1", &problems).unwrap();
        let loaded = load_snapshot(&root, "task-1");
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].fingerprint, problems[0].fingerprint);
        std::fs::remove_dir_all(&root).ok();
    }
}
