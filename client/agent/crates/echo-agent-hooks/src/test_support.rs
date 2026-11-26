//! Test-only helpers shared across `echo-agent-hooks` unit + integration tests.
//!
//! This module is gated on `#[cfg(test)]` and is exported as `pub(crate)`
//! so any in-crate `#[cfg(test)] mod tests` can use it. Integration tests
//! under `tests/` cannot reach it; for those, copy or re-implement the
//! handful of functions here that they need (the only one currently used
//! by integration tests is unrelated).

use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};

/// Run `f` with the env var `name` set to `value` (or unset if `value`
/// is `None`), restoring the previous value on return.
///
/// Uses `catch_unwind` so a panic inside `f` does not leak the env var
/// into the rest of the test process.
///
/// `cargo test` runs tests in parallel by default. Process env vars are
/// process-global, so callers should pick uniquely-named vars to avoid
/// inter-test races. The lifecycle here (save -> set -> run -> restore)
/// is panic-safe but not race-safe.
///
/// **FOLLOW-UP**: the helper does not
/// enforce the unique-name discipline -- a future contributor passing
/// a common name like `HOME` could trigger flaky tests. The standard
/// fix is to add `serial_test` as a dev-dep and decorate every
/// env-touching test with `#[serial(env_var)]` so the test runner
/// serialises them. For now the unique-name
/// convention plus `catch_unwind` restoration is sufficient for the
/// tests that ship today.
pub(crate) fn with_env_var<R>(name: &str, value: Option<&str>, f: impl FnOnce() -> R) -> R {
    let previous = std::env::var_os(name);
    // SAFETY: env-var writes are not thread-safe. Callers use uniquely
    // named vars so no concurrent test races on the same name.
    unsafe {
        match value {
            Some(v) => std::env::set_var(name, v),
            None => std::env::remove_var(name),
        }
    }

    let result = catch_unwind(AssertUnwindSafe(f));

    // SAFETY: see above. Restore unconditionally so a panic doesn't
    // leak env state to subsequent tests.
    unsafe {
        match previous {
            Some(prev) => std::env::set_var(name, prev),
            None => std::env::remove_var(name),
        }
    }

    match result {
        Ok(value) => value,
        Err(payload) => resume_unwind(payload),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restores_previous_value_on_normal_return() {
        let key = "ECHO_AGENT_HOOKS_TEST_SUPPORT_RESTORE";
        with_env_var(key, Some("first"), || {
            with_env_var(key, Some("second"), || {
                assert_eq!(std::env::var(key).unwrap(), "second");
            });
            assert_eq!(std::env::var(key).unwrap(), "first");
        });
        assert!(std::env::var(key).is_err());
    }

    #[test]
    fn restores_previous_unset_state_on_normal_return() {
        let key = "ECHO_AGENT_HOOKS_TEST_SUPPORT_UNSET_RESTORE";
        // SAFETY: see module-level note.
        unsafe {
            std::env::remove_var(key);
        }
        with_env_var(key, Some("temporary"), || {
            assert_eq!(std::env::var(key).unwrap(), "temporary");
        });
        assert!(std::env::var(key).is_err());
    }

    #[test]
    fn restores_after_panic() {
        let key = "ECHO_AGENT_HOOKS_TEST_SUPPORT_PANIC_RESTORE";
        // SAFETY: see module-level note.
        unsafe {
            std::env::remove_var(key);
        }
        let panicked = catch_unwind(AssertUnwindSafe(|| {
            with_env_var(key, Some("during-panic"), || {
                panic!("intentional");
            });
        }));
        assert!(panicked.is_err(), "expected panic to propagate");
        assert!(
            std::env::var(key).is_err(),
            "env var must be restored after panic"
        );
    }

    #[test]
    fn allows_explicit_unset() {
        let key = "ECHO_AGENT_HOOKS_TEST_SUPPORT_EXPLICIT_UNSET";
        // SAFETY: see module-level note.
        unsafe {
            std::env::set_var(key, "before");
        }
        with_env_var(key, None, || {
            assert!(std::env::var(key).is_err());
        });
        assert_eq!(std::env::var(key).unwrap(), "before");
        // SAFETY: see module-level note.
        unsafe {
            std::env::remove_var(key);
        }
    }
}
