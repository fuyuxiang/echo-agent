//! Headless mode (`echoagent -p`) test runner.
//!
//! Runs the echoagent binary as a subprocess with the mock server, captures output.

use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::time::Duration;

use tokio::io::AsyncReadExt as _;

use crate::env::echo_agent_binary;
use crate::mock_server::MockInferenceServer;
use crate::process::{TestOutput, TestProcess, TestProcessConfig};
use crate::sandbox::TestSandbox;

pub struct HeadlessResult {
    pub status: ExitStatus,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
    /// Wall time of the headless command invocation; logged so CI timeout
    /// budgets can be tuned against observed durations.
    pub elapsed: Duration,
}

/// Timeout for one headless echoagent invocation: 60 seconds, multiplied by
/// [`crate::scaled`]'s `ECHO_AGENT_TEST_TIMEOUT_SCALE`.
fn headless_timeout() -> Duration {
    crate::scaled(Duration::from_secs(60))
}

const HEADLESS_DRAIN_TIMEOUT: Duration = Duration::from_secs(2);

/// Run `echoagent` with the given args against the mock server, bounded by the
/// scaled headless timeout. Uses an isolated HOME and disables telemetry.
pub async fn run_headless(
    server: &MockInferenceServer,
    args: &[&str],
    cwd: &Path,
) -> HeadlessResult {
    run_headless_with_env(server, args, cwd, &[]).await
}

/// Like [`run_headless`], but with extra environment variables applied after the
/// sandbox baseline so they take precedence — e.g. to re-enable a feature the
/// baseline turns off.
pub async fn run_headless_with_env(
    server: &MockInferenceServer,
    args: &[&str],
    cwd: &Path,
    env: &[(&str, &str)],
) -> HeadlessResult {
    let sandbox = TestSandbox::builder().mock_url(server.url()).build();
    let mut cmd = tokio::process::Command::new(echo_agent_binary());
    cmd.args(args).current_dir(cwd);
    run_headless_with_cmd_and_sandbox(cmd, &sandbox, env).await
}

/// Apply and retain one [`TestSandbox`] while running a custom headless command.
pub async fn run_headless_in_sandbox(
    cmd: tokio::process::Command,
    sandbox: TestSandbox,
) -> HeadlessResult {
    run_headless_in_sandbox_with_env(cmd, sandbox, &[]).await
}

pub async fn run_headless_in_sandbox_with_env(
    cmd: tokio::process::Command,
    sandbox: TestSandbox,
    overrides: &[(&str, &str)],
) -> HeadlessResult {
    run_headless_in_sandbox_borrowed_with_env(cmd, &sandbox, overrides).await
}

/// Run a custom headless command while leaving the caller's sandbox available
/// for post-run artifact inspection.
pub async fn run_headless_in_sandbox_borrowed(
    cmd: tokio::process::Command,
    sandbox: &TestSandbox,
) -> HeadlessResult {
    run_headless_in_sandbox_borrowed_with_env(cmd, sandbox, &[]).await
}

pub async fn run_headless_in_sandbox_borrowed_with_env(
    cmd: tokio::process::Command,
    sandbox: &TestSandbox,
    overrides: &[(&str, &str)],
) -> HeadlessResult {
    run_headless_with_cmd_and_sandbox(cmd, sandbox, overrides).await
}

async fn run_headless_with_cmd_and_sandbox(
    cmd: tokio::process::Command,
    sandbox: &TestSandbox,
    overrides: &[(&str, &str)],
) -> HeadlessResult {
    let program = PathBuf::from(cmd.as_std().get_program());
    let started = std::time::Instant::now();
    let mut process = TestProcess::spawn(
        cmd,
        sandbox,
        TestProcessConfig::new()
            .label(format!("headless command {}", program.display()))
            .stdout(TestOutput::Piped)
            .stderr(TestOutput::Piped)
            .envs(overrides.iter().copied()),
    )
    .unwrap_or_else(|error| {
        panic!(
            "failed to spawn headless command at {}: {error}\n{}",
            program.display(),
            sandbox.diagnostic_summary(),
        )
    });

    let mut stdout = process.take_stdout().expect("child stdout missing");
    let stdout_handle = tokio::spawn(async move {
        let mut bytes = Vec::new();
        stdout.read_to_end(&mut bytes).await?;
        Ok::<Vec<u8>, std::io::Error>(bytes)
    });
    let mut stderr = process.take_stderr().expect("child stderr missing");
    let stderr_handle = tokio::spawn(async move {
        let mut bytes = Vec::new();
        stderr.read_to_end(&mut bytes).await?;
        Ok::<Vec<u8>, std::io::Error>(bytes)
    });

    let (status, timed_out) = match process
        .wait_with_deadline(headless_timeout())
        .await
        .unwrap_or_else(|error| {
            panic!(
                "failed to wait for headless command {}: {error}\n{}",
                program.display(),
                process.diagnostic_summary(),
            )
        }) {
        Some(status) => (status, false),
        None => {
            let status = process.kill().await.unwrap_or_else(|error| {
                panic!(
                    "failed to kill timed out headless command {}: {error}\n{}",
                    program.display(),
                    process.diagnostic_summary(),
                )
            });
            (status, true)
        }
    };

    let stdout = finish_output_drain(
        stdout_handle,
        process.stdout_tail().text,
        "stdout",
        &program,
        &process,
    )
    .await;
    let stderr = finish_output_drain(
        stderr_handle,
        process.stderr_tail().text,
        "stderr",
        &program,
        &process,
    )
    .await;

    let elapsed = started.elapsed();
    // Timing breadcrumb for tuning CI timeout budgets against observed
    // durations (visible with --nocapture).
    eprintln!(
        "[harness-timing] headless command {}: {elapsed:?} (timed_out={timed_out})",
        program.display()
    );

    HeadlessResult {
        status,
        stdout,
        stderr,
        timed_out,
        elapsed,
    }
}

async fn finish_output_drain(
    mut handle: tokio::task::JoinHandle<std::io::Result<Vec<u8>>>,
    partial_tail: String,
    stream: &str,
    program: &Path,
    process: &TestProcess,
) -> String {
    match tokio::time::timeout(HEADLESS_DRAIN_TIMEOUT, &mut handle).await {
        Ok(Ok(Ok(bytes))) => String::from_utf8_lossy(&bytes).into_owned(),
        Ok(Ok(Err(error))) => {
            tracing::warn!(
                %stream,
                program = %program.display(),
                %error,
                "headless output drain failed; returning captured partial tail"
            );
            partial_tail
        }
        Ok(Err(error)) => {
            tracing::warn!(
                %stream,
                program = %program.display(),
                %error,
                "headless output task failed; returning captured partial tail"
            );
            partial_tail
        }
        Err(_) => {
            handle.abort();
            let _ = handle.await;
            tracing::warn!(
                %stream,
                program = %program.display(),
                diagnostics = %process.diagnostic_summary(),
                "headless output drain timed out; returning captured partial tail"
            );
            partial_tail
        }
    }
}

const CRASH_PATTERNS: &[&str] = &[
    "panicked at",
    "SIGSEGV",
    "segfault",
    "undefined symbol",
    "SIGABRT",
    "cannot open shared object",
];

/// Diagnostic helper: format the tail of stderr for assertion messages.
pub fn stderr_tail(stderr: &str, max_chars: usize) -> &str {
    &stderr[stderr.len().saturating_sub(max_chars)..]
}

/// Assert that a headless run succeeded (non-timeout, zero exit code).
pub fn assert_headless_success(
    result: &HeadlessResult,
    label: &str,
    server: Option<&MockInferenceServer>,
) {
    assert!(
        !result.timed_out,
        "{label}: timed out after {:?}\nstderr tail:\n{}",
        headless_timeout(),
        stderr_tail(&result.stderr, 500)
    );
    assert!(
        result.status.success(),
        "{label}: exited with {:?}\nstderr tail:\n{}\n{}",
        result.status.code(),
        stderr_tail(&result.stderr, 1000),
        server
            .map(|s| format!("request log:\n{}", s.request_log_summary()))
            .unwrap_or_default()
    );
}

/// Panic if stderr contains any crash/linking-failure indicators.
pub fn assert_no_crashes(stderr: &str) {
    let lower = stderr.to_lowercase();
    for pattern in CRASH_PATTERNS {
        assert!(
            !lower.contains(&pattern.to_lowercase()),
            "stderr contains crash indicator '{pattern}':\n{}",
            stderr_tail(stderr, 500)
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[tokio::test]
    async fn borrowed_runner_keeps_sandbox_artifacts_available() {
        let sandbox = TestSandbox::new();
        let artifact = sandbox.temp_dir().join("borrowed-headless.txt");
        let script = format!("printf kept > '{}'", artifact.display());
        let mut cmd = tokio::process::Command::new("/bin/sh");
        cmd.args(["-c", &script]);

        let result = run_headless_in_sandbox_borrowed(cmd, &sandbox).await;

        assert!(result.status.success(), "stderr: {}", result.stderr);
        assert_eq!(std::fs::read_to_string(artifact).unwrap(), "kept");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn output_drain_timeout_returns_partial_capture() {
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let handle = tokio::spawn(async move {
            let _keep_open = tx;
            let _ = rx.await;
            Ok::<Vec<u8>, std::io::Error>(b"complete".to_vec())
        });
        let sandbox = TestSandbox::new();
        let mut command = tokio::process::Command::new("/bin/sh");
        command.args(["-c", "exit 0"]);
        let mut process = TestProcess::spawn(
            command,
            &sandbox,
            TestProcessConfig::new().label("headless-drain-test"),
        )
        .expect("spawn drain fixture");
        process
            .wait_with_deadline(Duration::from_secs(2))
            .await
            .expect("wait fixture")
            .expect("fixture exits");

        let output = finish_output_drain(
            handle,
            "partial".to_owned(),
            "stdout",
            Path::new("fixture"),
            &process,
        )
        .await;
        assert_eq!(output, "partial");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn custom_headless_env_is_explicit_and_wins_after_sandbox_baseline() {
        let sandbox = TestSandbox::new();
        let mut cmd = tokio::process::Command::new("/bin/sh");
        cmd.args([
            "-c",
            "printf '%s|%s|%s|%s' \"${AMBIENT_ONLY-unset}\" \"$ECHO_AGENT_PROMPT_SUGGESTIONS\" \"$FEATURE_TEST_VAR\" \"$HOME\"",
        ])
        .env("AMBIENT_ONLY", "discarded")
        .env("ECHO_AGENT_PROMPT_SUGGESTIONS", "command-level-discarded");

        let result = run_headless_in_sandbox_borrowed_with_env(
            cmd,
            &sandbox,
            &[
                ("ECHO_AGENT_PROMPT_SUGGESTIONS", "explicit-override"),
                ("FEATURE_TEST_VAR", "enabled"),
            ],
        )
        .await;

        assert!(result.status.success(), "stderr: {}", result.stderr);
        assert_eq!(
            result.stdout,
            format!(
                "unset|explicit-override|enabled|{}",
                sandbox.home().display()
            )
        );
    }
}
