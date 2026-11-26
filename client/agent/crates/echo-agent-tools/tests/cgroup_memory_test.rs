//! Integration test for cgroup memory-high OOM handling.
//!
//! **Must be run on Linux with cgroupv2** and sufficient permissions to create
//! child cgroups (typically root, or a user-session cgroup with delegation).
//!
//! Run with:
//! ```bash
//! # On a Linux machine (as root or with cgroup delegation):
//! cargo test -p echo-agent-tools --test cgroup_memory_test -- --ignored --nocapture
//!
//! # If you need root:
//! sudo -E cargo test -p echo-agent-tools --test cgroup_memory_test -- --ignored --nocapture
//! ```
//!
//! The cgroup-dependent tests (1–5) are `#[ignore]`d by default so they don't
//! run in CI where cgroup delegation is typically unavailable.  Test 6 (no-config)
//! always runs.
//!
//! The tests exercise:
//! 1. A command that stays under the memory limit → exits normally (exit 0)
//! 2. A command that exceeds memory.high → killed with exit 137, signal "oom"
//! 3. The session (backend) survives an OOM and can run another command after
//! 4. Background tasks are also killed on OOM
//! 5. A gradual allocator that slowly ramps up past the limit

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use echo_agent_tools::computer::local::LocalTerminalBackend;
use echo_agent_tools::computer::local::cgroup::{CgroupMemoryConfig, PROCESS_OOM_EXIT_CODE};
use echo_agent_tools::computer::types::{TerminalBackend, TerminalRunRequest, TerminalRunResult};
use echo_agent_tools::notification::types::ToolNotificationHandle;

// ── Helpers ──────────────────────────────────────────────────────────────

/// Small memory limit for testing: 32 MiB high, 32 MiB headroom (64 MiB hard max).
fn test_memory_config() -> CgroupMemoryConfig {
    CgroupMemoryConfig {
        memory_high_bytes: 32 * 1024 * 1024, // 32 MiB
        headroom_bytes: 32 * 1024 * 1024,    // 32 MiB headroom → 64 MiB hard max
    }
}

fn make_request(command: &str, timeout_secs: u64) -> TerminalRunRequest {
    let output_file = std::env::temp_dir().join(format!(
        "cgroup-test-{}-{}.out",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));

    TerminalRunRequest {
        command: command.to_string(),
        working_directory: PathBuf::from("/tmp"),
        env: HashMap::new(),
        timeout: Duration::from_secs(timeout_secs),
        output_byte_limit: 1024 * 1024,
        output_file,
        notification_handle: ToolNotificationHandle::noop(),
        tool_call_id: format!("cgroup-test-{}", uuid::Uuid::now_v7()),
        display_command: None,
        auto_background_on_timeout: false,
        foreground_block_budget: None,
        kind: Default::default(),
        owner_session_id: None,
        description: None,
    }
}

fn is_linux_with_cgroupv2() -> bool {
    #[cfg(target_os = "linux")]
    {
        // Check that cgroupv2 is mounted
        std::path::Path::new("/sys/fs/cgroup/cgroup.controllers").exists()
    }
    #[cfg(not(target_os = "linux"))]
    {
        false
    }
}

fn can_create_cgroups() -> bool {
    if !is_linux_with_cgroupv2() {
        return false;
    }
    // Try reading our own cgroup path — if this works, we can probably create children
    #[cfg(target_os = "linux")]
    {
        if let Ok(contents) = std::fs::read_to_string("/proc/self/cgroup") {
            for line in contents.lines() {
                if let Some(path) = line.strip_prefix("0::") {
                    let cgroup_dir = std::path::PathBuf::from(format!("/sys/fs/cgroup{}", path));
                    // Check if we can write to this cgroup's subtree_control
                    let subtree = cgroup_dir.join("cgroup.subtree_control");
                    return subtree.exists();
                }
            }
        }
        false
    }
    #[cfg(not(target_os = "linux"))]
    {
        false
    }
}

fn skip_unless_cgroup() {
    if !can_create_cgroups() {
        eprintln!(
            "\n╔══════════════════════════════════════════════════════════════╗\n\
               ║  SKIPPED: cgroupv2 not available or insufficient perms.     ║\n\
               ║  Run on Linux as root or with cgroup delegation.            ║\n\
               ╚══════════════════════════════════════════════════════════════╝\n"
        );
    }
}

fn print_result(label: &str, result: &TerminalRunResult) {
    let output_preview = if result.combined_output.len() > 200 {
        format!("{}…", &result.combined_output[..200])
    } else {
        result.combined_output.clone()
    };
    eprintln!(
        "\n── {label} ──\n  exit_code: {:?}\n  signal:    {:?}\n  timed_out: {}\n  truncated: {}\n  output:    {:?}\n",
        result.exit_code,
        result.signal,
        result.timed_out,
        result.truncated,
        output_preview.trim(),
    );
}

// ── Tests ────────────────────────────────────────────────────────────────

/// Test 1: A command that stays well under the limit exits normally.
#[tokio::test]
#[ignore = "requires Linux cgroupv2 with delegation — run with: cargo test --test cgroup_memory_test -- --ignored --nocapture"]
async fn test_under_limit_exits_normally() {
    skip_unless_cgroup();
    if !can_create_cgroups() {
        return;
    }

    eprintln!("\n=== Test: under_limit_exits_normally ===");
    let backend = LocalTerminalBackend::with_memory_limit(test_memory_config());
    // Give actor time to initialize cgroup
    tokio::time::sleep(Duration::from_millis(200)).await;

    let result = backend
        .run(make_request(
            "echo 'hello from cgroup'; cat /proc/self/cgroup",
            10,
        ))
        .await
        .expect("command should succeed");

    print_result("Under limit", &result);

    assert_eq!(result.exit_code, Some(0), "Expected exit code 0");
    assert!(
        result.combined_output.contains("hello from cgroup"),
        "Output should contain our echo"
    );
    assert_ne!(
        result.signal.as_deref(),
        Some("oom"),
        "Should NOT be OOM-killed"
    );

    eprintln!("✅ PASSED: under_limit_exits_normally");
}

/// Test 2: A command that allocates way more than the limit is killed with 137/oom.
#[tokio::test]
#[ignore = "requires Linux cgroupv2 with delegation"]
async fn test_over_limit_gets_oom_killed() {
    skip_unless_cgroup();
    if !can_create_cgroups() {
        return;
    }

    eprintln!("\n=== Test: over_limit_gets_oom_killed ===");
    let backend = LocalTerminalBackend::with_memory_limit(test_memory_config());
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Allocate 128 MiB in Python — well above the 32 MiB high / 64 MiB max limits.
    let alloc_cmd = r#"python3 -c "
import sys
print('Allocating 128 MiB...', flush=True)
data = bytearray(128 * 1024 * 1024)
print('Allocation succeeded (should not reach here)', flush=True)
""#;

    let result = backend
        .run(make_request(alloc_cmd, 30))
        .await
        .expect("command should return a result (even if killed)");

    print_result("Over limit", &result);

    // The process should be killed — either by our monitor (exit 137 + signal "oom")
    // or by the kernel hard OOM killer (exit 137 / signal 9).
    let killed_by_memory = result.exit_code == Some(PROCESS_OOM_EXIT_CODE)
        || result.signal.as_deref() == Some("oom")
        || result
            .signal
            .as_ref()
            .is_some_and(|s| s.contains("signal 9"));

    assert!(
        killed_by_memory,
        "Expected OOM kill (exit 137 or signal 9/oom), got exit_code={:?} signal={:?}",
        result.exit_code, result.signal
    );

    // Output before the kill should be preserved
    assert!(
        result.combined_output.contains("Allocating 128 MiB"),
        "Output before OOM should be captured"
    );

    eprintln!("✅ PASSED: over_limit_gets_oom_killed");
}

/// Test 3: After an OOM, the backend still works for subsequent commands.
#[tokio::test]
#[ignore = "requires Linux cgroupv2 with delegation"]
async fn test_session_survives_oom() {
    skip_unless_cgroup();
    if !can_create_cgroups() {
        return;
    }

    eprintln!("\n=== Test: session_survives_oom ===");
    let backend = LocalTerminalBackend::with_memory_limit(test_memory_config());
    tokio::time::sleep(Duration::from_millis(200)).await;

    // First: trigger an OOM
    let oom_cmd = r#"python3 -c "data = bytearray(128 * 1024 * 1024)""#;
    let oom_result = backend
        .run(make_request(oom_cmd, 30))
        .await
        .expect("should return result even on OOM");

    print_result("OOM command", &oom_result);

    // Small delay so cgroup memory is reclaimed
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Second: run a lightweight command — should succeed
    let ok_result = backend
        .run(make_request("echo 'alive after OOM'", 10))
        .await
        .expect("post-OOM command should succeed");

    print_result("After OOM", &ok_result);

    assert_eq!(
        ok_result.exit_code,
        Some(0),
        "Post-OOM command should exit 0"
    );
    assert!(
        ok_result.combined_output.contains("alive after OOM"),
        "Post-OOM output should contain our echo"
    );

    eprintln!("✅ PASSED: session_survives_oom");
}

/// Test 4: Background tasks are also subject to the memory limit.
#[tokio::test]
#[ignore = "requires Linux cgroupv2 with delegation"]
async fn test_background_task_oom() {
    skip_unless_cgroup();
    if !can_create_cgroups() {
        return;
    }

    eprintln!("\n=== Test: background_task_oom ===");
    let backend = LocalTerminalBackend::with_memory_limit(test_memory_config());
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Start a background command that will OOM
    let alloc_cmd = r#"python3 -c "
import time
print('BG: allocating...', flush=True)
time.sleep(0.5)
data = bytearray(128 * 1024 * 1024)
print('BG: done (should not reach here)', flush=True)
time.sleep(60)
""#;

    let handle = backend
        .run_background(make_request(alloc_cmd, 60))
        .await
        .expect("background spawn should succeed");

    eprintln!("  Background task_id: {}", handle.task_id);

    // Wait for completion (it should be killed before the 60s timeout)
    let snapshot = backend
        .wait_for_completion(&handle.task_id, Some(Duration::from_secs(30)))
        .await;

    if let Some(snap) = &snapshot {
        eprintln!(
            "  BG result: completed={} exit_code={:?} signal={:?} output={:?}",
            snap.completed,
            snap.exit_code,
            snap.signal,
            &snap.output[..snap.output.len().min(200)]
        );

        assert!(snap.completed, "Background task should have completed");
        let killed_by_memory = snap.exit_code == Some(PROCESS_OOM_EXIT_CODE)
            || snap.signal.as_deref() == Some("oom")
            || snap.signal.as_ref().is_some_and(|s| s.contains("signal 9"));
        assert!(
            killed_by_memory,
            "Background task should be OOM-killed, got exit_code={:?} signal={:?}",
            snap.exit_code, snap.signal
        );
    } else {
        panic!("Expected a snapshot for the background task");
    }

    eprintln!("✅ PASSED: background_task_oom");
}

/// Test 5: Gradual allocation that slowly ramps past the limit.
/// This tests that the inotify monitor catches the memory.high event
/// rather than relying on the kernel's hard memory.max kill.
#[tokio::test]
#[ignore = "requires Linux cgroupv2 with delegation"]
async fn test_gradual_allocation_oom() {
    skip_unless_cgroup();
    if !can_create_cgroups() {
        return;
    }

    eprintln!("\n=== Test: gradual_allocation_oom ===");
    let backend = LocalTerminalBackend::with_memory_limit(test_memory_config());
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Allocate in 1 MiB chunks with a small delay — slowly ramps past 32 MiB.
    let gradual_cmd = r#"python3 -c "
import time, sys
chunks = []
for i in range(128):
    chunks.append(bytearray(1024 * 1024))  # 1 MiB per chunk
    print(f'Allocated {i+1} MiB', flush=True)
    time.sleep(0.05)
print('Finished all allocations (should not reach here)', flush=True)
""#;

    let result = backend
        .run(make_request(gradual_cmd, 30))
        .await
        .expect("should return result");

    print_result("Gradual allocation", &result);

    let killed_by_memory = result.exit_code == Some(PROCESS_OOM_EXIT_CODE)
        || result.signal.as_deref() == Some("oom")
        || result
            .signal
            .as_ref()
            .is_some_and(|s| s.contains("signal 9"));

    assert!(
        killed_by_memory,
        "Expected OOM kill for gradual allocator, got exit_code={:?} signal={:?}",
        result.exit_code, result.signal
    );

    // Should have some output showing allocations before the kill
    assert!(
        result.combined_output.contains("Allocated"),
        "Should see some allocation progress before kill"
    );

    // Should NOT have finished all 128 MiB
    assert!(
        !result.combined_output.contains("Finished all allocations"),
        "Should have been killed before finishing"
    );

    eprintln!("✅ PASSED: gradual_allocation_oom");
}

/// Test 6: No memory config → no cgroup enforcement, large alloc succeeds.
/// This verifies the no-op path works correctly.
#[tokio::test]
async fn test_no_config_no_enforcement() {
    eprintln!("\n=== Test: no_config_no_enforcement ===");

    // Use the plain `new()` constructor — no memory limits
    let backend = LocalTerminalBackend::new();
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Allocate 64 MiB — would be killed with a 32 MiB limit, but should succeed here
    let alloc_cmd = r#"python3 -c "
data = bytearray(64 * 1024 * 1024)
print('Allocated 64 MiB without limits')
""#;

    let result = backend
        .run(make_request(alloc_cmd, 10))
        .await
        .expect("command should succeed without limits");

    print_result("No enforcement", &result);

    assert_eq!(result.exit_code, Some(0), "Should exit 0 without limits");
    assert!(
        result.combined_output.contains("Allocated 64 MiB"),
        "Allocation should succeed without limits"
    );

    eprintln!("✅ PASSED: no_config_no_enforcement");
}
