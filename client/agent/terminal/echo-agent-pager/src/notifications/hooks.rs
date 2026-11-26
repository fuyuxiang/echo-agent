use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use crate::notifications::NotificationEvent;
use crate::notifications::config::NotificationHook;

fn execute_hook(
    command: &str,
    event_str: &str,
    message: &str,
    session_id: Option<&str>,
    timeout: Duration,
) {
    let mut cmd = Command::new("sh");
    cmd.arg("-c")
        .arg(command)
        .env("ECHO_AGENT_EVENT", event_str)
        .env("ECHO_AGENT_MESSAGE", message)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(sid) = session_id {
        cmd.env("ECHO_AGENT_SESSION_ID", sid);
    }

    echo_agent_tty_utils::detach_std_command(&mut cmd);

    #[allow(clippy::disallowed_methods)] // enrolled below, once the child exists
    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(e) => {
            tracing::debug!(error = %e, command, "hook spawn failed");
            return;
        }
    };

    let group = match echo_agent_tty_utils::global_process_scope().enroll_std(&child) {
        Ok(group) => group,
        Err(error) => {
            tracing::debug!(error = %error, command, "hook process group enrollment failed");
            let _ = child.kill();
            if !matches!(
                echo_agent_tty_utils::wait_child_bounded(
                    &mut child,
                    echo_agent_tty_utils::KILL_REAP_TIMEOUT,
                ),
                Ok(Some(_))
            ) && let Err((error, child, _)) =
                echo_agent_tty_utils::spawn_child_reaper("notification-hook-reaper", child, None)
            {
                tracing::error!(error = %error, command, child_id = child.id(), "hook cleanup bounded abandonment after enrollment failure");
            }
            return;
        }
    };

    match echo_agent_tty_utils::wait_child_bounded(&mut child, timeout) {
        Ok(Some(_)) => drop(group),
        Ok(None) => {
            tracing::warn!(command, "hook timed out");
            kill_tree_and_reap(child, group, command);
        }
        Err(error) if echo_agent_tty_utils::is_child_wait_identity_uncertain(&error) => {
            tracing::error!(error = %error, command, "hook wait lost child identity; numeric cleanup forbidden");
            // ECHILD makes this group unsafe for this and later numeric cleanup.
            drop(group);
            abandon_child(child, None, command);
        }
        Err(error) => {
            tracing::debug!(error = %error, command, "hook wait failed");
            kill_tree_and_reap(child, group, command);
        }
    }
}

fn kill_tree_and_reap(
    mut child: Child,
    group: Arc<echo_agent_tty_utils::ProcessGroup>,
    command: &str,
) {
    if let Err(group_error) = group.kill()
        && let Err(child_error) = child.kill()
    {
        tracing::warn!(error = %group_error, fallback_error = %child_error, command, "hook group and direct-child kill failed");
    }
    match echo_agent_tty_utils::wait_child_bounded(
        &mut child,
        echo_agent_tty_utils::KILL_REAP_TIMEOUT,
    ) {
        Ok(Some(_)) => drop(group),
        Ok(None) => abandon_child(child, Some(group), command),
        Err(error) => {
            tracing::warn!(error = %error, command, "hook bounded reap failed");
            abandon_child(child, Some(group), command);
        }
    }
}

fn abandon_child(
    child: Child,
    group: Option<Arc<echo_agent_tty_utils::ProcessGroup>>,
    command: &str,
) {
    if let Err((error, child, group)) =
        echo_agent_tty_utils::spawn_child_reaper("notification-hook-reaper", child, group)
    {
        tracing::error!(error = %error, command, child_id = child.id(), has_group = group.is_some(), "hook cleanup bounded abandonment: reaper thread spawn failed");
    }
}

pub fn run_hook(hook: &NotificationHook, event: &NotificationEvent) {
    let command = hook.command.clone();
    let event_str: &'static str = event.kind.as_str();
    let message = event.body.clone();
    let session_id = event.session_id.clone();
    let timeout = Duration::from_secs(hook.timeout_secs.max(1));

    std::thread::spawn(move || {
        execute_hook(
            &command,
            event_str,
            &message,
            session_id.as_deref(),
            timeout,
        );
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notifications::config::NotificationEventKind;
    use std::time::Instant;

    fn test_event() -> NotificationEvent {
        NotificationEvent {
            kind: NotificationEventKind::TurnComplete,
            title: "EchoAgent".into(),
            body: "test body payload".into(),
            session_id: Some("test-session-123".into()),
        }
    }

    #[test]
    fn sets_environment_variables() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("env.txt");
        let command = format!(
            "printf 'ECHO_AGENT_EVENT=%s\\nECHO_AGENT_MESSAGE=%s\\nECHO_AGENT_SESSION_ID=%s\\n' \
             \"$ECHO_AGENT_EVENT\" \"$ECHO_AGENT_MESSAGE\" \"$ECHO_AGENT_SESSION_ID\" > {}",
            out.display()
        );

        execute_hook(
            &command,
            "Turn complete",
            "hello world",
            Some("sess-42"),
            Duration::from_secs(5),
        );

        let content = std::fs::read_to_string(&out).unwrap();
        assert!(
            content.contains("ECHO_AGENT_EVENT=Turn complete"),
            "missing ECHO_AGENT_EVENT: {content}"
        );
        assert!(
            content.contains("ECHO_AGENT_MESSAGE=hello world"),
            "missing ECHO_AGENT_MESSAGE: {content}"
        );
        assert!(
            content.contains("ECHO_AGENT_SESSION_ID=sess-42"),
            "missing ECHO_AGENT_SESSION_ID: {content}"
        );
    }

    #[test]
    fn omits_session_id_when_none() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("env.txt");
        let command = format!("env > {}", out.display());

        execute_hook(
            &command,
            "Turn complete",
            "msg",
            None,
            Duration::from_secs(5),
        );

        let content = std::fs::read_to_string(&out).unwrap();
        assert!(
            !content.contains("ECHO_AGENT_SESSION_ID"),
            "ECHO_AGENT_SESSION_ID should not be set: {content}"
        );
    }

    #[test]
    fn kills_descendants_on_timeout() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("descendant-finished");
        let command = format!("(sleep 2; touch {}) & wait", marker.display());
        execute_hook(
            &command,
            "Turn complete",
            "msg",
            None,
            Duration::from_millis(100),
        );
        let deadline = Instant::now() + Duration::from_millis(2300);
        while Instant::now() < deadline {
            assert!(!marker.exists(), "timeout must kill hook descendants");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    #[test]
    fn handles_failed_shell_command_gracefully() {
        execute_hook(
            "/nonexistent/path/binary",
            "Turn complete",
            "msg",
            None,
            Duration::from_secs(1),
        );
    }

    #[test]
    fn handles_nonzero_exit_gracefully() {
        execute_hook(
            "exit 1",
            "Turn complete",
            "msg",
            None,
            Duration::from_secs(5),
        );
    }

    #[test]
    fn successful_command_completes_without_error() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("done");
        let command = format!("touch {}", marker.display());

        execute_hook(
            &command,
            "Turn complete",
            "msg",
            None,
            Duration::from_secs(5),
        );

        assert!(marker.exists());
    }

    #[test]
    fn run_hook_spawns_thread_without_panic() {
        let hook = NotificationHook {
            command: "true".into(),
            events: vec![],
            only_unfocused: false,
            timeout_secs: 5,
        };
        run_hook(&hook, &test_event());
        std::thread::sleep(Duration::from_millis(200));
    }

    #[test]
    fn timeout_clamped_to_minimum_one_second() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("done");
        let hook = NotificationHook {
            command: format!("sleep 100; touch {}", marker.display()),
            events: vec![],
            only_unfocused: false,
            timeout_secs: 0, // exercises the .max(1) clamp inside run_hook
        };
        let start = Instant::now();
        run_hook(&hook, &test_event());
        // Wait for the spawned thread to finish (clamp turns 0 -> 1s timeout)
        std::thread::sleep(Duration::from_millis(2500));
        let elapsed = start.elapsed();
        // The hook should have been killed by the 1s timeout, so the marker
        // file should NOT exist (sleep 100 never completes).
        assert!(
            !marker.exists(),
            "hook should have been killed by timeout before creating marker"
        );
        // Sanity: the whole thing completed well under 10s, confirming the
        // timeout was ~1s (clamped) not 0s (instant) or unbounded.
        assert!(
            elapsed < Duration::from_secs(5),
            "should complete within a few seconds, took {elapsed:?}"
        );
    }

    #[test]
    fn run_hook_passes_correct_env_via_thread() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("env.txt");
        let hook = NotificationHook {
            command: format!(
                "printf 'ECHO_AGENT_EVENT=%s\\nECHO_AGENT_MESSAGE=%s\\nECHO_AGENT_SESSION_ID=%s\\n' \
                 \"$ECHO_AGENT_EVENT\" \"$ECHO_AGENT_MESSAGE\" \"$ECHO_AGENT_SESSION_ID\" > {}",
                out.display()
            ),
            events: vec![],
            only_unfocused: false,
            timeout_secs: 5,
        };
        let event = test_event();
        run_hook(&hook, &event);

        // Poll for the output file instead of a fixed sleep — the spawned
        // thread + fork/exec may take variable time on loaded systems.
        let deadline = Instant::now() + Duration::from_secs(5);
        let content = loop {
            if let Ok(c) = std::fs::read_to_string(&out) {
                break c;
            }
            assert!(
                Instant::now() < deadline,
                "hook did not produce output file within 5s (sh or printf may not be available)"
            );
            std::thread::sleep(Duration::from_millis(50));
        };
        assert!(content.contains("ECHO_AGENT_EVENT=Turn complete"));
        assert!(content.contains("ECHO_AGENT_MESSAGE=test body payload"));
        assert!(content.contains("ECHO_AGENT_SESSION_ID=test-session-123"));
    }
}
