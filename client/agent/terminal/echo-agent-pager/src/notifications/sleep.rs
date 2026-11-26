//! Platform-specific sleep prevention.
//!
//! Prevents the machine from idle-sleeping while an agent turn is in progress.
//! macOS uses IOKit power assertions; Linux spawns `systemd-inhibit`.
//!
//! Threading: lives on `AppView` (single-threaded, `!Send`).
//! Uses `Cell`/`RefCell` instead of atomics/mutexes.

use std::cell::Cell;

/// Prevents idle sleep while an agent turn is running.
///
/// Calls are idempotent: repeated `inhibit()` or `release()` calls are no-ops
/// when already in the requested state. On `Drop`, any held assertion is released.
pub struct SleepInhibitor {
    #[cfg(target_os = "macos")]
    assertion_id: Cell<Option<u32>>,
    #[cfg(target_os = "linux")]
    child: std::cell::RefCell<Option<std::process::Child>>,
    active: Cell<bool>,
    /// Set on first `platform_inhibit` failure to avoid repeated spawn
    /// attempts on platforms where the inhibitor is unavailable (e.g.
    /// containers without systemd-inhibit).
    platform_unavailable: Cell<bool>,
    enabled: bool,
}

impl SleepInhibitor {
    pub fn new(enabled: bool) -> Self {
        Self {
            #[cfg(target_os = "macos")]
            assertion_id: Cell::new(None),
            #[cfg(target_os = "linux")]
            child: std::cell::RefCell::new(None),
            active: Cell::new(false),
            platform_unavailable: Cell::new(false),
            enabled,
        }
    }

    /// Prevent idle sleep. No-op if already inhibiting, disabled, or
    /// platform support was already determined to be unavailable.
    pub fn inhibit(&self) {
        if !self.enabled || self.active.get() || self.platform_unavailable.get() {
            return;
        }
        if self.platform_inhibit() {
            self.active.set(true);
        } else {
            self.platform_unavailable.set(true);
        }
    }

    /// Allow idle sleep again. No-op if not currently inhibiting.
    pub fn release(&self) {
        if !self.active.get() {
            return;
        }
        self.platform_release();
        self.active.set(false);
    }

    #[cfg(target_os = "macos")]
    fn platform_inhibit(&self) -> bool {
        let mut assertion_id: u32 = 0;
        let reason = core_foundation::string::CFString::new("echoagent: agent turn in progress");
        let assertion_type =
            core_foundation::string::CFString::from_static_string("NoIdleSleepAssertion");

        // IOPMAssertionCreateWithName returns kIOReturnSuccess (0) on success.
        let result = unsafe {
            IOPMAssertionCreateWithName(
                assertion_type.as_concrete_TypeRef(),
                255, // kIOPMAssertionLevelOn
                reason.as_concrete_TypeRef(),
                &mut assertion_id,
            )
        };

        if result == 0 {
            self.assertion_id.set(Some(assertion_id));
            true
        } else {
            tracing::warn!(error_code = result, "failed to create IOPMAssertion");
            false
        }
    }

    #[cfg(target_os = "macos")]
    fn platform_release(&self) {
        if let Some(id) = self.assertion_id.get() {
            let result = unsafe { IOPMAssertionRelease(id) };
            if result != 0 {
                tracing::warn!(error_code = result, "failed to release IOPMAssertion");
            }
            self.assertion_id.set(None);
        }
    }

    #[cfg(target_os = "linux")]
    fn platform_inhibit(&self) -> bool {
        let mut cmd = std::process::Command::new("systemd-inhibit");
        cmd.args([
            "--what=idle",
            "--who=echoagent",
            "--why=agent turn in progress",
            "sleep",
            "infinity",
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
        echo_agent_tty_utils::detach_std_command(&mut cmd);
        // The spawned process is the lock holder: `systemd-inhibit` keeps
        // the idle-inhibit fd itself and runs `sleep infinity` as its child
        // — it is the same pid `release()` SIGTERMs on a clean turn end.
        // Bind that pid to us so a crashed/killed echoagent (SIGKILL,
        // `panic=abort` SIGABRT — no Drop runs) can't leave an immortal
        // inhibitor holding the lock and pid slots on shared hosts.
        echo_agent_tty_utils::kill_on_parent_death_std(&mut cmd);
        #[allow(clippy::disallowed_methods)] // bound by kill-on-parent-death; released each turn
        let result = cmd.spawn();

        match result {
            Ok(child) => {
                *self.child.borrow_mut() = Some(child);
                true
            }
            Err(e) => {
                tracing::debug!(error = %e, "systemd-inhibit not available");
                false
            }
        }
    }

    #[cfg(target_os = "linux")]
    fn platform_release(&self) {
        if let Some(mut child) = self.child.borrow_mut().take() {
            let _ = nix::sys::signal::kill(
                nix::unistd::Pid::from_raw(child.id() as i32),
                nix::sys::signal::Signal::SIGTERM,
            );
            let _ = child.wait();
        }
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    fn platform_inhibit(&self) -> bool {
        false
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    fn platform_release(&self) {}
}

impl Drop for SleepInhibitor {
    fn drop(&mut self) {
        self.release();
    }
}

// -- macOS IOKit FFI ---------------------------------------------------------

#[cfg(target_os = "macos")]
use core_foundation::base::TCFType;

#[cfg(target_os = "macos")]
#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IOPMAssertionCreateWithName(
        assertion_type: core_foundation::string::CFStringRef,
        assertion_level: u32,
        reason_for_activity: core_foundation::string::CFStringRef,
        assertion_id: *mut u32,
    ) -> i32;

    fn IOPMAssertionRelease(assertion_id: u32) -> i32;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_inhibitor_is_noop() {
        let inhibitor = SleepInhibitor::new(false);
        inhibitor.inhibit();
        assert!(!inhibitor.active.get());
        inhibitor.release();
        assert!(!inhibitor.active.get());
    }

    /// On unsupported platforms (not macOS/Linux), platform_inhibit returns
    /// false and `platform_unavailable` latches to prevent retry spam.
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    #[test]
    fn platform_unavailable_prevents_retries() {
        let inhibitor = SleepInhibitor::new(true);
        // First attempt: platform_inhibit fails, flag latches.
        inhibitor.inhibit();
        assert!(!inhibitor.active.get());
        assert!(inhibitor.platform_unavailable.get());
        // Subsequent calls are short-circuited by the flag.
        inhibitor.inhibit();
        assert!(!inhibitor.active.get());
    }

    #[test]
    fn inhibit_is_idempotent() {
        let inhibitor = SleepInhibitor::new(true);
        inhibitor.inhibit();
        let was_active = inhibitor.active.get();
        // Second call should not change state (and not spawn a second child/assertion).
        inhibitor.inhibit();
        assert_eq!(inhibitor.active.get(), was_active);
        inhibitor.release();
    }

    #[test]
    fn release_is_idempotent() {
        let inhibitor = SleepInhibitor::new(true);
        // Release without prior inhibit is a no-op.
        inhibitor.release();
        assert!(!inhibitor.active.get());
        // Double release after inhibit is safe.
        inhibitor.inhibit();
        inhibitor.release();
        assert!(!inhibitor.active.get());
        inhibitor.release();
        assert!(!inhibitor.active.get());
    }

    #[test]
    fn inhibit_release_cycle() {
        let inhibitor = SleepInhibitor::new(true);
        inhibitor.inhibit();
        // On Linux, this spawns systemd-inhibit; on other platforms it's a no-op.
        // Either way, the active flag tracks the intent.
        let was_active = inhibitor.active.get();
        inhibitor.release();
        assert!(!inhibitor.active.get());
        // Can re-inhibit after release.
        inhibitor.inhibit();
        assert_eq!(inhibitor.active.get(), was_active);
        inhibitor.release();
    }

    #[test]
    fn drop_releases() {
        let inhibitor = SleepInhibitor::new(true);
        inhibitor.inhibit();
        drop(inhibitor);
        // No assertion — just verify no panic/leak.
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_inhibit_spawns_child() {
        let inhibitor = SleepInhibitor::new(true);
        inhibitor.inhibit();
        if inhibitor.active.get() {
            assert!(inhibitor.child.borrow().is_some());
            inhibitor.release();
            assert!(inhibitor.child.borrow().is_none());
        }
        // If systemd-inhibit isn't available, active stays false — that's fine.
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_release_kills_child() {
        let inhibitor = SleepInhibitor::new(true);
        inhibitor.inhibit();
        if inhibitor.active.get() {
            // Grab the pid before release.
            let pid = inhibitor.child.borrow().as_ref().map(|c| c.id());
            assert!(pid.is_some());
            inhibitor.release();
            assert!(inhibitor.child.borrow().is_none());
            assert!(!inhibitor.active.get());
        }
    }
}
