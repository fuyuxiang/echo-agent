//! Smoke test for sandbox enforcement.
//!
//! This binary applies a sandbox profile and then attempts various operations
//! to verify kernel enforcement. Run it directly to test:
//!
//! ```bash
//! # Test workspace profile (should allow writes to CWD, block ~/Desktop)
//! cargo run -p echo-agent-sandbox --example sandbox_smoke_test
//!
//! # Test strict profile
//! cargo run -p echo-agent-sandbox --example sandbox_smoke_test -- strict
//!
//! # Test read-only profile
//! cargo run -p echo-agent-sandbox --example sandbox_smoke_test -- read-only
//! ```

use echo_agent_sandbox::{ProfileName, SandboxManager};
use std::path::Path;

fn main() {
    // Parse profile from args (default: workspace).
    let profile_name = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "workspace".to_string());

    let profile: ProfileName = profile_name.parse().unwrap_or_else(|e| {
        eprintln!("Error: {e}");
        std::process::exit(1);
    });

    // Check platform support before applying
    let support = SandboxManager::support_info();
    println!(
        "Platform support: {}",
        if support.is_supported { "YES" } else { "NO" }
    );
    println!("Details: {}", support.details);

    if !support.is_supported {
        println!("\n⚠️  Sandbox not supported on this platform.");
        println!("   On macOS: Seatbelt should be available (10.5+)");
        println!("   On Linux: Landlock requires kernel ≥ 5.13");
        println!("\n   Tests will show what WOULD happen, but won't enforce.");
    }

    let workspace = std::env::current_dir().expect("failed to get cwd");
    println!("\nProfile:      {profile}");
    println!("Workspace:    {}", workspace.display());

    // Apply the sandbox
    println!("\n--- Applying sandbox ---");
    let mut sandbox = SandboxManager::new(profile, &workspace);
    match sandbox.apply(&workspace) {
        Ok(()) => {
            if sandbox.is_applied() {
                println!("✅ Sandbox applied (kernel-enforced, irreversible)");
            } else {
                println!("⚠️  Sandbox was not applied (unsupported platform or Off profile)");
            }
        }
        Err(e) => {
            println!("❌ Sandbox apply failed: {e}");
        }
    }

    println!(
        "Child network restricted: {}",
        sandbox.restrict_child_network()
    );

    // Test operations
    println!("\n--- Testing filesystem operations ---\n");

    // Test 1: Read CWD (should always work)
    test_read("Read CWD", &workspace);

    // Test 2: Read /tmp (should work for workspace/read-only)
    test_read("Read /tmp", Path::new("/tmp"));

    // Test 3: Read home directory (should work for workspace/read-only, blocked for strict)
    if let Some(home) = dirs::home_dir() {
        test_read("Read ~/", &home);
    }

    // Test 4: Write to CWD (should work for workspace/strict, blocked for read-only)
    let test_file = workspace.join(".sandbox-test-write");
    test_write("Write to CWD", &test_file);
    // Clean up
    let _ = std::fs::remove_file(&test_file);

    // Test 5: Write to /tmp (should work for workspace/strict, blocked for read-only)
    let tmp_test = Path::new("/tmp/.echo-agent-sandbox-test");
    test_write("Write to /tmp", tmp_test);
    let _ = std::fs::remove_file(tmp_test);

    // Test 6: Write outside workspace (should be blocked for all active profiles)
    if let Some(home) = dirs::home_dir() {
        let outside = home.join(".sandbox-test-blocked");
        test_write("Write to ~/", &outside);
        let _ = std::fs::remove_file(&outside);
    }

    // Test 7: Read ~/.ssh (a custom profile's `deny` list could block this)
    if let Some(home) = dirs::home_dir() {
        let ssh = home.join(".ssh");
        if ssh.exists() {
            test_read("Read ~/.ssh/", &ssh);
        }
    }

    // Summary
    println!("\n--- Sandbox event log ---");
    let events = sandbox.logger().take_events();
    for event in &events {
        println!(
            "  {:?}: {} {:?}",
            event.event_type, event.profile, event.target
        );
    }
    if events.is_empty() {
        println!("  (no events recorded)");
    }

    println!("\n✅ Smoke test complete");
}

fn test_read(label: &str, path: &Path) {
    if path.is_file() {
        match std::fs::read(path) {
            Ok(_) => println!("  ✅ {label}: OK (read)"),
            Err(e)
                if e.raw_os_error() == Some(libc::EACCES)
                    || e.raw_os_error() == Some(libc::EPERM) =>
            {
                println!("  🔒 {label}: BLOCKED ({e})");
            }
            Err(e) => println!("  ❌ {label}: ERROR ({e})"),
        }
        return;
    }
    match std::fs::read_dir(path) {
        Ok(mut entries) => {
            let count = entries.by_ref().take(5).count();
            println!("  ✅ {label}: OK ({count} entries)");
        }
        Err(e) => {
            if e.raw_os_error() == Some(libc::EACCES) || e.raw_os_error() == Some(libc::EPERM) {
                println!("  🔒 {label}: BLOCKED ({e})");
            } else {
                println!("  ❌ {label}: ERROR ({e})");
            }
        }
    }
}

fn test_write(label: &str, path: &Path) {
    match std::fs::write(path, b"sandbox-test") {
        Ok(()) => {
            println!("  ✅ {label}: OK (written)");
        }
        Err(e) => {
            if e.raw_os_error() == Some(libc::EACCES) || e.raw_os_error() == Some(libc::EPERM) {
                println!("  🔒 {label}: BLOCKED ({e})");
            } else {
                println!("  ❌ {label}: ERROR ({e})");
            }
        }
    }
}
