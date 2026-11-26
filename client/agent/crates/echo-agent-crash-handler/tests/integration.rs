//! Integration tests for echo-agent-crash-handler.
//!
//! These tests verify that installing the crash handler does not interfere
//! with normal program operation (tokio runtime, signal handling, I/O),
//! and that it correctly captures crash data when a fatal signal fires.
//!
//! Tests that send fatal signals use subprocess isolation: the test process
//! re-executes itself with an env var that selects the crash scenario, so
//! the parent can verify outcomes without dying.

#![cfg(unix)]

use std::path::Path;
use std::process::Command;

/// Re-invoke the current test binary as a subprocess with the given scenario.
/// Returns (exit status, stdout, stderr).
fn run_scenario(scenario: &str, crash_dir: &Path) -> (std::process::ExitStatus, String, String) {
    let exe = std::env::current_exe().expect("current_exe");
    let output = Command::new(exe)
        .env("CRASH_TEST_SCENARIO", scenario)
        .env("CRASH_TEST_DIR", crash_dir.as_os_str())
        .arg("--ignored")
        .arg("--exact")
        .arg("--nocapture")
        .arg("subprocess_entry")
        .output()
        .expect("failed to spawn subprocess");
    (
        output.status,
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

// ── Subprocess entry point ──────────────────────────────────────────────

/// This test is `#[ignore]`d so it only runs when invoked as a subprocess
/// by the parent test via `run_scenario`. The `CRASH_TEST_SCENARIO` env
/// var selects which scenario to execute.
#[test]
#[ignore]
fn subprocess_entry() {
    let scenario = match std::env::var("CRASH_TEST_SCENARIO") {
        Ok(s) => s,
        Err(_) => return, // not a subprocess invocation
    };
    let crash_dir = std::env::var("CRASH_TEST_DIR").expect("CRASH_TEST_DIR");
    let crash_dir = std::path::PathBuf::from(crash_dir);

    // Install the crash handler before anything else.
    let config = echo_agent_crash_handler::CrashHandlerConfig {
        app_version: "0.0.0-test".to_string(),
        crash_dir,
    };
    echo_agent_crash_handler::install(config);

    match scenario.as_str() {
        // Scenario 1: install handler, run tokio runtime with concurrent work, exit cleanly.
        "tokio_normal" => {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("tokio runtime");
            rt.block_on(async {
                // Spawn several concurrent tasks to stress the runtime.
                let mut handles = Vec::new();
                for i in 0..20 {
                    handles.push(tokio::spawn(async move {
                        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                        i * i
                    }));
                }
                let mut sum = 0u64;
                for h in handles {
                    sum += h.await.unwrap();
                }
                // Also test signal infrastructure coexistence.
                // Register a tokio SIGTERM handler (same as the pager does).
                #[cfg(unix)]
                {
                    use tokio::signal::unix::{SignalKind, signal};
                    let _term = signal(SignalKind::terminate())
                        .expect("tokio SIGTERM handler should work alongside crash handler");
                }
                eprintln!("tokio_normal: sum={sum}, all tasks completed");
            });
        }

        // Scenario 2: install handler, do sync file I/O and computation, exit cleanly.
        "sync_normal" => {
            let tmp = tempfile::tempdir().expect("tempdir");
            for i in 0..50 {
                let path = tmp.path().join(format!("file-{i}.txt"));
                std::fs::write(&path, format!("contents {i}")).expect("write");
                let data = std::fs::read_to_string(&path).expect("read");
                assert!(data.contains(&format!("{i}")));
            }
            eprintln!("sync_normal: 50 files written and read back");
        }

        // Scenario 3: install handler, send ourselves SIGBUS, verify crash file written.
        "sigbus" => {
            // Give the handler a moment to be fully installed, then crash.
            unsafe { libc::raise(libc::SIGBUS) };
        }

        // Scenario 4: install handler, send ourselves SIGSEGV.
        "sigsegv" => {
            unsafe { libc::raise(libc::SIGSEGV) };
        }

        // Scenario 6: install handler, abort. This is the path every Rust
        // panic takes in release builds (panic = "abort" → SIGABRT).
        "sigabrt" => {
            std::process::abort();
        }

        // Scenario 5: tokio runtime + signal coexistence, then clean shutdown.
        "tokio_signals" => {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("tokio runtime");
            rt.block_on(async {
                use tokio::signal::unix::{SignalKind, signal};
                let mut usr1 = signal(SignalKind::user_defined1()).expect("SIGUSR1 handler");

                // Send ourselves SIGUSR1 and verify tokio receives it
                // (proves our SIGBUS/SIGSEGV handler doesn't clobber other signals).
                unsafe { libc::raise(libc::SIGUSR1) };
                tokio::time::timeout(std::time::Duration::from_secs(2), usr1.recv())
                    .await
                    .expect("SIGUSR1 should arrive within 2s");

                eprintln!("tokio_signals: SIGUSR1 received, signal coexistence OK");
            });
        }

        other => {
            eprintln!("unknown scenario: {other}");
            std::process::exit(99);
        }
    }
}

// ── Parent test cases ───────────────────────────────────────────────────

#[test]
fn handler_does_not_interfere_with_tokio_runtime() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (status, _stdout, stderr) = run_scenario("tokio_normal", tmp.path());
    assert!(
        status.success(),
        "tokio_normal should exit 0, got {status:?}\nstderr: {stderr}"
    );
    assert!(
        stderr.contains("all tasks completed"),
        "should see completion message\nstderr: {stderr}"
    );
    // No crash file should exist.
    assert!(
        !tmp.path().join("last-crash.bin").exists()
            || std::fs::metadata(tmp.path().join("last-crash.bin"))
                .map(|m| m.len() == 0)
                .unwrap_or(true),
        "crash file should not contain data after clean exit"
    );
}

#[test]
fn handler_does_not_interfere_with_sync_io() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (status, _stdout, stderr) = run_scenario("sync_normal", tmp.path());
    assert!(
        status.success(),
        "sync_normal should exit 0, got {status:?}\nstderr: {stderr}"
    );
    assert!(
        stderr.contains("50 files written"),
        "should see completion message\nstderr: {stderr}"
    );
}

#[test]
fn handler_does_not_clobber_other_signal_handlers() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (status, _stdout, stderr) = run_scenario("tokio_signals", tmp.path());
    assert!(
        status.success(),
        "tokio_signals should exit 0, got {status:?}\nstderr: {stderr}"
    );
    assert!(
        stderr.contains("signal coexistence OK"),
        "SIGUSR1 should be delivered through tokio\nstderr: {stderr}"
    );
}

#[test]
fn sigbus_produces_valid_crash_blob() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (status, _stdout, _stderr) = run_scenario("sigbus", tmp.path());

    // Process should have been killed by a signal.
    // We expect SIGBUS, but the frame-pointer walker may hit unmapped memory
    // and cause a secondary SIGSEGV (SA_RESETHAND ensures it terminates).
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        let sig = status.signal();
        assert!(
            sig == Some(libc::SIGBUS) || sig == Some(libc::SIGSEGV),
            "process should be killed by SIGBUS or SIGSEGV, got signal={sig:?} status={status:?}"
        );
    }

    // The crash file should be parseable.
    let crash_file = tmp.path().join("last-crash.bin");
    assert!(crash_file.exists(), "crash file should exist after SIGBUS");
    let data = std::fs::read(&crash_file).expect("read crash file");
    assert!(
        data.len() > 4,
        "crash file should have data, got {} bytes",
        data.len()
    );

    let blob =
        echo_agent_crash_handler::format::CrashBlob::parse(&data).expect("crash blob should parse");

    // On macOS SIGBUS=10, on Linux SIGBUS=7, SIGSEGV=11 on both.
    // The frame-pointer walker may cause a secondary SIGSEGV.
    assert!(
        blob.signal == 7 || blob.signal == 10 || blob.signal == 11,
        "signal should be SIGBUS or SIGSEGV, got {}",
        blob.signal
    );
    assert_eq!(blob.app_version, "0.0.0-test");
    assert!(blob.pid > 0, "PID should be nonzero");
    assert!(blob.timestamp > 0, "timestamp should be nonzero");

    // check_previous_crash should produce a report.
    let report = echo_agent_crash_handler::check_previous_crash(tmp.path())
        .expect("should produce a crash report");
    assert!(report.signal_name.contains("SIGBUS"));
    assert_eq!(report.app_version, "0.0.0-test");
    assert!(report.report_path.exists(), "report file should be written");

    // Crash blob should be consumed (deleted).
    assert!(
        !crash_file.exists(),
        "crash file should be deleted after processing"
    );
}

#[test]
fn sigsegv_produces_valid_crash_blob() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (status, _stdout, _stderr) = run_scenario("sigsegv", tmp.path());

    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        let sig = status.signal();
        assert_eq!(
            sig,
            Some(libc::SIGSEGV),
            "process should be killed by SIGSEGV, got signal={sig:?} status={status:?}"
        );
    }

    let crash_file = tmp.path().join("last-crash.bin");
    assert!(crash_file.exists(), "crash file should exist after SIGSEGV");
    let data = std::fs::read(&crash_file).expect("read crash file");
    let blob =
        echo_agent_crash_handler::format::CrashBlob::parse(&data).expect("crash blob should parse");
    assert_eq!(blob.signal, 11, "signal should be SIGSEGV (11)");
    assert_eq!(blob.app_version, "0.0.0-test");
}

#[test]
fn sigabrt_produces_valid_crash_blob() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (status, _stdout, _stderr) = run_scenario("sigabrt", tmp.path());

    // The handler must re-raise with default disposition so the process
    // still dies with SIGABRT semantics. The frame-pointer walker may hit
    // unmapped memory and cause a secondary SIGSEGV (as in the SIGBUS test).
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        let sig = status.signal();
        assert!(
            sig == Some(libc::SIGABRT) || sig == Some(libc::SIGSEGV),
            "process should be killed by SIGABRT (or a secondary SIGSEGV), got signal={sig:?} status={status:?}"
        );
    }

    let crash_file = tmp.path().join("last-crash.bin");
    assert!(crash_file.exists(), "crash file should exist after abort()");
    let data = std::fs::read(&crash_file).expect("read crash file");
    let blob =
        echo_agent_crash_handler::format::CrashBlob::parse(&data).expect("crash blob should parse");
    assert_eq!(
        blob.signal, 6,
        "signal should be SIGABRT (6), got {}",
        blob.signal
    );
    assert_eq!(blob.app_version, "0.0.0-test");
    assert!(blob.pid > 0, "PID should be nonzero");
    assert!(blob.timestamp > 0, "timestamp should be nonzero");

    // check_previous_crash should produce a SIGABRT-labelled report.
    let report = echo_agent_crash_handler::check_previous_crash(tmp.path())
        .expect("should produce a crash report");
    assert!(
        report.signal_name.contains("SIGABRT"),
        "report should name SIGABRT, got {}",
        report.signal_name
    );
    assert_eq!(report.app_version, "0.0.0-test");
    assert!(report.report_path.exists(), "report file should be written");
    assert!(
        !crash_file.exists(),
        "crash file should be deleted after processing"
    );
}

#[test]
fn clean_exit_does_not_produce_crash_report() {
    let tmp = tempfile::tempdir().expect("tempdir");
    // Run both scenarios and verify no crash artifacts.
    for scenario in &["tokio_normal", "sync_normal", "tokio_signals"] {
        let (status, _stdout, stderr) = run_scenario(scenario, tmp.path());
        assert!(status.success(), "{scenario} failed: {stderr}");
    }
    // check_previous_crash should return None.
    let report = echo_agent_crash_handler::check_previous_crash(tmp.path());
    assert!(report.is_none(), "no crash report after clean exits");
}
