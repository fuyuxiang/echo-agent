// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;

/// `validate_requirements` runs at the very top of `main` (before the runtime/TUI, so fd 2 is the real
/// terminal): an invalid `fail_closed` version_override must abort startup with the update/admin guidance and
/// exit 2 — distinct from the gate's exit 1.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "PTY e2e; run the owning pty_e2e_* Cargo test with --ignored (see Cargo.toml)"]
async fn requirements_version_failure_exits_2_with_guidance() {
    let sandbox = echo_agent_test_support::TestSandbox::new();
    let home_path = sandbox.echo_agent_home();
    // fail_closed + a version_override whose version can't parse → apply_version_overrides errs → startup aborts.
    std::fs::write(
        home_path.join("requirements.toml"),
        "fail_closed = true\n\n[[version_overrides]]\nminimum_version = \"not-a-version\"\n",
    )
    .expect("write requirements.toml");

    let binary = pager_binary().expect("resolve pager binary");
    let mut harness = PtyHarness::new_in_sandbox_ops(
        &binary,
        DEFAULT_ROWS,
        DEFAULT_COLS,
        &["--no-auto-update"],
        &sandbox,
        &[EnvOp::set("NO_COLOR", "1")],
        None,
    )
    .expect("spawn pager");

    let msg = "Update EchoAgent to a version the policy allows";
    // This single budget must cover the child's cold *exec* (a ~680MB unoptimized
    // debug binary) plus reaching `main()` and running `validate_requirements` —
    // all before the runtime/TUI. Under the ~60-way-parallel `pty_e2e` suite,
    // spawning that many large binaries at once can delay a child reaching its
    // first write well past 30s: the observed flake was an empty screen AND empty
    // raw output (the child had produced no bytes yet), not a wrong exit/guidance.
    // Widen to 120s (matching the suite's other contention-sensitive budgets) so a
    // genuinely slow cold start still lands while a truly hung startup still fails.
    let deadline = Instant::now() + Duration::from_secs(120);
    let mut exit_code = None;
    while Instant::now() < deadline {
        harness.update(Duration::from_millis(100));
        // Guidance already visible while the child is still exiting: capture the
        // exit code (the child exits right after printing) and stop.
        if harness.contains_text(msg) || String::from_utf8_lossy(harness.raw_output()).contains(msg)
        {
            if exit_code.is_none() {
                match wait_for_exit_status(&mut harness, Duration::from_secs(2))
                    .expect("wait for requirements exit")
                {
                    PtyExitPoll::Exited(code) => exit_code = Some(code),
                    PtyExitPoll::Running | PtyExitPoll::PendingStatus => {}
                }
            }
            break;
        }
        if exit_code.is_none() {
            match wait_for_exit_status(&mut harness, Duration::ZERO)
                .expect("poll requirements exit")
            {
                PtyExitPoll::Exited(code) => exit_code = Some(code),
                PtyExitPoll::Running | PtyExitPoll::PendingStatus => {}
            }
            if exit_code.is_some() {
                // The child exited before the guidance surfaced on our side.
                // It wrote the guidance to fd 2 just before exiting; keep
                // draining until the PTY reader delivers those buffered bytes
                // (it will, then hit EOF) instead of a single fixed window. A
                // one-shot post-exit drain can be starved under the ~60-way
                // parallel suite, losing the bytes — the observed flake was an
                // empty screen AND empty raw output.
                let drain_deadline = Instant::now() + Duration::from_secs(10);
                while !(harness.contains_text(msg)
                    || String::from_utf8_lossy(harness.raw_output()).contains(msg))
                    && Instant::now() < drain_deadline
                {
                    harness.update(Duration::from_millis(100));
                }
                break;
            }
        }
    }

    let raw = String::from_utf8_lossy(harness.raw_output()).into_owned();
    assert!(
        harness.contains_text(msg) || raw.contains(msg),
        "requirements-version failure must print the update/admin guidance; screen:\n{}\nraw:\n{raw}",
        harness.screen_contents()
    );
    assert_eq!(
        exit_code,
        Some(2),
        "an invalid fail_closed requirements layer must exit 2; got {exit_code:?}\nraw:\n{raw}"
    );
}
