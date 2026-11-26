use std::collections::HashSet;
use std::process::{Command, Stdio};

fn pager_binary() -> Result<std::path::PathBuf, String> {
    for key in ["PAGER_BINARY", "CARGO_BIN_EXE_echo-agent-pager"] {
        if let Some(value) = std::env::var_os(key) {
            let path = std::path::PathBuf::from(value);
            if path.exists() {
                return Ok(path);
            }
        }
    }
    Err("PAGER_BINARY/CARGO_BIN_EXE_echo-agent-pager not set".to_owned())
}

#[test]
#[ignore = "spawns the real pager binary; CI/Bazel provides PAGER_BINARY"]
fn doctor_json_bypasses_unrelated_startup_state() {
    let binary = pager_binary().expect("real pager binary is required when this test is selected");
    let temp = tempfile::tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let echo_agent_home = temp.path().join("echo-agent-home");
    std::fs::create_dir_all(&home).expect("create HOME");
    std::fs::create_dir_all(&echo_agent_home).expect("create ECHO_AGENT_HOME");

    let version_path = echo_agent_home.join("version.json");
    std::fs::write(
        &version_path,
        br#"{"stable":{"version":"999.0.0"},"checked_at":0}"#,
    )
    .expect("write valid hostile version state");

    let before = directory_entries(&echo_agent_home);
    let output = run_pager(
        &binary,
        &home,
        &echo_agent_home,
        "/bin/sh",
        &["doctor", "--json"],
        &[],
    );

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stderr.is_empty(),
        "stderr must be clean: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let json: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("stdout is one JSON document");
    assert_eq!(json["schemaVersion"], "1");
    assert!(!String::from_utf8_lossy(&output.stdout).contains("EchoAgent Doctor"));

    let after = directory_entries(&echo_agent_home);
    assert_eq!(after, before, "doctor must not create startup artifacts");
    assert_eq!(
        std::fs::read(&version_path).unwrap(),
        br#"{"stable":{"version":"999.0.0"},"checked_at":0}"#
    );
    for absent in ["docs", "crash", "memtrace", "active_sessions.json"] {
        assert!(
            !echo_agent_home.join(absent).exists(),
            "unexpected startup artifact: {absent}"
        );
    }
}

#[test]
#[ignore = "spawns the real pager binary; CI/Bazel provides PAGER_BINARY"]
fn doctor_fix_without_id_lists_tmux_fixes_from_current_probe_evidence() {
    let binary = pager_binary().expect("real pager binary is required when selected");
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let echo_agent_home = temp.path().join("qhome");
    let fake_bin = temp.path().join("bin");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&echo_agent_home).unwrap();
    std::fs::create_dir_all(&fake_bin).unwrap();
    let tmux = fake_bin.join("tmux");
    std::fs::write(
        &tmux,
        "#!/bin/sh\ncase \"$*\" in\n  *\"show-option -gv allow-passthrough\"*) exit 0;;\n  *\"show-option -gqv extended-keys\"*) printf off;;\n  *\"show-option -gqv allow-passthrough\"*) printf off;;\n  *\"show-option -gqv set-clipboard\"*) printf off;;\n  *\"display-message\"*) printf x;;\n  *) exit 1;;\nesac\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&tmux, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let output = run_pager(
        &binary,
        &home,
        &echo_agent_home,
        "/bin/bash",
        &["doctor", "fix"],
        &[
            ("TMUX", "/tmp/tmux/default,1,0"),
            ("PATH", fake_bin.to_str().unwrap()),
        ],
    );
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    for handle in ["tmux-clipboard", "dcs-passthrough"] {
        assert!(stdout.contains(handle), "{stdout}");
    }
    assert!(!stdout.contains("Set up local SSH wrapping"), "{stdout}");
}

#[test]
#[ignore = "spawns the real pager binary; CI/Bazel provides PAGER_BINARY"]
fn doctor_tmux_fix_probes_are_bounded_and_never_write_on_timeout() {
    let binary = pager_binary().expect("real pager binary is required when selected");
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let echo_agent_home = temp.path().join("qhome");
    let fake_bin = temp.path().join("bin");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&echo_agent_home).unwrap();
    std::fs::create_dir_all(&fake_bin).unwrap();
    let tmux = fake_bin.join("tmux");
    std::fs::write(&tmux, "#!/bin/sh\nsleep 30\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&tmux, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    for args in [
        ["doctor", "fix", "", ""],
        ["doctor", "fix", "tmux-clipboard", "--yes"],
    ] {
        let actual = args
            .iter()
            .copied()
            .filter(|value| !value.is_empty())
            .collect::<Vec<_>>();
        let started = std::time::Instant::now();
        let output = run_pager(
            &binary,
            &home,
            &echo_agent_home,
            "/bin/bash",
            &actual,
            &[
                ("TMUX", "/tmp/tmux/default,1,0"),
                ("PATH", fake_bin.to_str().unwrap()),
            ],
        );
        assert!(started.elapsed() < std::time::Duration::from_secs(12));
        if actual.len() == 2 {
            assert!(output.status.success());
            assert_eq!(output.stdout, b"No automatic fixes are available here.\n");
        } else {
            assert_eq!(output.status.code(), Some(1));
        }
        assert!(!home.join(".tmux.conf").exists());
    }
}

#[test]
#[ignore = "spawns the real pager binary; CI/Bazel provides PAGER_BINARY"]
fn doctor_tmux_fix_kills_background_pipe_holders_after_leader_exit() {
    let binary = pager_binary().expect("real pager binary is required when selected");
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let echo_agent_home = temp.path().join("qhome");
    let fake_bin = temp.path().join("bin");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&echo_agent_home).unwrap();
    std::fs::create_dir_all(&fake_bin).unwrap();
    let tmux = fake_bin.join("tmux");
    std::fs::write(&tmux, "#!/bin/sh\nsleep 30 &\nexit 0\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&tmux, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    for args in [
        vec!["doctor", "fix"],
        vec!["doctor", "fix", "tmux-clipboard", "--yes"],
    ] {
        let started = std::time::Instant::now();
        let output = run_pager(
            &binary,
            &home,
            &echo_agent_home,
            "/bin/bash",
            &args,
            &[
                ("TMUX", "/tmp/tmux/default,1,0"),
                ("PATH", fake_bin.to_str().unwrap()),
            ],
        );
        assert!(started.elapsed() < std::time::Duration::from_secs(12));
        if args.len() == 2 {
            assert!(output.status.success());
        } else {
            assert_eq!(output.status.code(), Some(1));
        }
        assert!(!home.join(".tmux.conf").exists());
    }
}

#[cfg(unix)]
#[test]
#[ignore = "spawns the real pager binary; CI/Bazel provides PAGER_BINARY"]
fn doctor_tmux_fix_kills_term_ignoring_redirected_descendants() {
    use std::os::unix::fs::PermissionsExt as _;

    let binary = pager_binary().expect("real pager binary is required when selected");
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let echo_agent_home = temp.path().join("qhome");
    let fake_bin = temp.path().join("bin");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&echo_agent_home).unwrap();
    std::fs::create_dir_all(&fake_bin).unwrap();
    let tmux = fake_bin.join("tmux");
    let pid_file = temp.path().join("descendant.pid");
    // Publish the descendant pid from the leader *before* it exits. Writing
    // `echo $$ > pid` inside the redirected child races doctor teardown:
    // `>` truncates first, then SIGKILL can land before the digits are written,
    // leaving an empty file (`ParseIntError { kind: Empty }`).
    std::fs::write(
        &tmux,
        format!(
            "#!/bin/sh\n( trap '' TERM; exec sleep 30 ) >/dev/null 2>&1 &\necho $! > '{}'\nexit 0\n",
            pid_file.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&tmux, std::fs::Permissions::from_mode(0o755)).unwrap();
    for args in [
        vec!["doctor", "fix"],
        vec!["doctor", "fix", "tmux-clipboard", "--yes"],
    ] {
        let _ = std::fs::remove_file(&pid_file);
        let output = run_pager(
            &binary,
            &home,
            &echo_agent_home,
            "/bin/bash",
            &args,
            &[
                ("TMUX", "/tmp/tmux/default,1,0"),
                ("PATH", fake_bin.to_str().unwrap()),
            ],
        );
        assert!(output.status.success() || output.status.code() == Some(1));
        let pid_raw = std::fs::read_to_string(&pid_file).unwrap_or_default();
        let pid: i32 = pid_raw.trim().parse().unwrap_or_else(|_| {
            panic!("descendant pid was not published before fake tmux exited: {pid_raw:?}")
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while std::time::Instant::now() < deadline {
            // SAFETY: kill(pid, 0) only probes liveness for the positive child PID.
            if unsafe { libc::kill(pid, 0) } != 0 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        // SAFETY: same liveness probe; ESRCH is the expected result.
        assert_ne!(
            unsafe { libc::kill(pid, 0) },
            0,
            "descendant {pid} survived"
        );
        assert!(!home.join(".tmux.conf").exists());
    }
}

#[test]
#[ignore = "spawns the real pager binary; CI/Bazel provides PAGER_BINARY"]
fn doctor_irrelevant_unsafe_byobu_does_not_break_ssh_or_plain_tmux() {
    let binary = pager_binary().expect("real pager binary is required when selected");
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let echo_agent_home = temp.path().join("qhome");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&echo_agent_home).unwrap();

    let ssh = run_pager(
        &binary,
        &home,
        &echo_agent_home,
        "/bin/bash",
        &["doctor", "fix", "ssh-wrap", "--yes"],
        &[("BYOBU_CONFIG_DIR", "relative")],
    );
    assert!(
        ssh.status.success(),
        "{}",
        String::from_utf8_lossy(&ssh.stderr)
    );
    assert!(home.join(".bashrc").exists());

    let plain = run_pager(
        &binary,
        &home,
        &echo_agent_home,
        "/bin/bash",
        &["doctor", "fix"],
        &[
            ("BYOBU_CONFIG_DIR", "relative"),
            ("TMUX", "/tmp/tmux/default,1,0"),
        ],
    );
    assert!(
        plain.status.success(),
        "{}",
        String::from_utf8_lossy(&plain.stderr)
    );
}

#[test]
#[ignore = "spawns the real pager binary; CI/Bazel provides PAGER_BINARY"]
fn doctor_hostile_home_and_byobu_create_no_config_files() {
    let binary =
        dunce::canonicalize(pager_binary().expect("real pager binary is required when selected"))
            .unwrap();
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let echo_agent_home = temp.path().join("qhome");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&echo_agent_home).unwrap();
    for (key, value) in [("HOME", "."), ("BYOBU_CONFIG_DIR", "relative")] {
        let mut command = base_pager_command(&binary, &home, &echo_agent_home, "/bin/bash");
        command
            .current_dir(temp.path())
            .env(key, value)
            .env("TMUX", "/tmp/tmux/default,1,0")
            .env("BYOBU_BACKEND", "tmux")
            .args(["doctor", "fix", "tmux-clipboard", "--yes"]);
        let output = command.output().unwrap();
        assert_eq!(
            output.status.code(),
            Some(1),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!temp.path().join(".tmux.conf").exists());
        assert!(!temp.path().join("relative/.tmux.conf").exists());
    }
}

#[test]
#[ignore = "spawns the real pager binary; CI/Bazel provides PAGER_BINARY"]
fn doctor_fix_without_id_lists_only_applicable_automatic_fixes() {
    let binary = pager_binary().expect("real pager binary is required when selected");
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let echo_agent_home = temp.path().join("qhome");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&echo_agent_home).unwrap();

    let output = run_pager(
        &binary,
        &home,
        &echo_agent_home,
        "/bin/bash",
        &["doctor", "fix"],
        &[("SSH_CONNECTION", "1 2 3 4")],
    );
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains("On your local computer, run: echoagent doctor fix ssh-wrap"),
        "{stdout}"
    );
    assert!(!home.join(".bashrc").exists());

    let output = run_pager(
        &binary,
        &home,
        &echo_agent_home,
        "/bin/bash",
        &["doctor", "fix", "terminal.ssh-wrap", "--yes"],
        &[],
    );
    assert!(output.status.success());
    let output = run_pager(
        &binary,
        &home,
        &echo_agent_home,
        "/bin/bash",
        &["doctor", "fix"],
        &[],
    );
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "No automatic fixes are available here.\n"
    );
}

#[test]
#[ignore = "spawns the real pager binary; CI/Bazel provides PAGER_BINARY"]
fn doctor_tmux_fix_yes_writes_only_actual_home_tmux_config() {
    let binary = pager_binary().expect("real pager binary is required when this test is selected");
    let temp = tempfile::tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let echo_agent_home = temp.path().join("echo-agent-home");
    let fake_bin = temp.path().join("bin");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&echo_agent_home).unwrap();
    std::fs::create_dir_all(&fake_bin).unwrap();
    let tmux = fake_bin.join("tmux");
    std::fs::write(
        &tmux,
        "#!/bin/sh\ncase \"$*\" in\n  *\"show-option -gv allow-passthrough\"*) exit 0;;\n  *\"show-option -gqv allow-passthrough\"*) printf off;;\n  *\"show-option -gqv set-clipboard\"*) printf off;;\n  *\"display-message\"*) printf x;;\n  *) exit 1;;\nesac\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&tmux, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    let output = run_pager(
        &binary,
        &home,
        &echo_agent_home,
        "/bin/bash",
        &["doctor", "fix", "tmux-clipboard", "--yes"],
        &[
            ("TMUX", "/tmp/tmux/default,1,0"),
            ("PATH", fake_bin.to_str().unwrap()),
        ],
    );
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains("Added `set -g set-clipboard on`"),
        "{stdout}"
    );
    assert!(
        stdout.contains("Reload tmux with `tmux source-file"),
        "{stdout}"
    );
    assert!(
        stdout.contains("Run /doctor again to verify the live setting"),
        "{stdout}"
    );
    assert_eq!(
        std::fs::read_to_string(home.join(".tmux.conf")).unwrap(),
        "# >>> echoagent doctor >>>\n# >>> terminal.tmux-clipboard >>>\nset -g set-clipboard on\n# <<< terminal.tmux-clipboard <<<\n# <<< echoagent doctor <<<"
    );
    assert!(!echo_agent_home.join(".tmux.conf").exists());
}

#[test]
#[ignore = "spawns the real pager binary; CI/Bazel provides PAGER_BINARY"]
fn doctor_fix_yes_writes_only_actual_home_shell_rc() {
    let binary = pager_binary().expect("real pager binary is required when this test is selected");
    let temp = tempfile::tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let echo_agent_home = temp.path().join("echo-agent-home");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&echo_agent_home).unwrap();

    let output = run_pager(
        &binary,
        &home,
        &echo_agent_home,
        "/bin/bash",
        &["doctor", "fix", "terminal.ssh-wrap", "--yes"],
        &[],
    );

    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("Fix: terminal.ssh-wrap"));
    assert!(stdout.contains("ssh -f"));
    assert!(stdout.contains("ControlPersist"));
    assert!(stdout.contains("~^Z"));
    assert!(stdout.contains("command ssh"));
    assert_eq!(
        std::fs::read_to_string(home.join(".bashrc")).unwrap(),
        "# >>> echoagent doctor >>>\n# >>> terminal.ssh-wrap >>>\nalias ssh='echoagent wrap ssh'\n# <<< terminal.ssh-wrap <<<\n# <<< echoagent doctor <<<"
    );
    assert!(!echo_agent_home.join(".bashrc").exists());
}

#[test]
#[ignore = "spawns the real pager binary; CI/Bazel provides PAGER_BINARY"]
fn doctor_fix_safety_boundaries_are_process_isolated() {
    let binary = pager_binary().expect("real pager binary is required when selected");
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let echo_agent_home = temp.path().join("qhome");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&echo_agent_home).unwrap();

    let conflict = home.join(".zshrc");
    std::fs::write(&conflict, "alias ssh='ssh -A'\n").unwrap();
    let output = run_pager(
        &binary,
        &home,
        &echo_agent_home,
        "/bin/zsh",
        &["doctor", "fix", "ssh-wrap", "--yes"],
        &[],
    );
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("EchoAgent found an existing SSH alias or function")
            && stderr.contains(&conflict.display().to_string()),
        "{stderr}"
    );
    assert_eq!(
        std::fs::read_to_string(&conflict).unwrap(),
        "alias ssh='ssh -A'\n"
    );
    std::fs::remove_file(&conflict).unwrap();

    let output = run_pager(
        &binary,
        &home,
        &echo_agent_home,
        "/bin/zsh",
        &["doctor", "fix", "ssh-wrap"],
        &[],
    );
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stdout).contains("Fix: terminal.ssh-wrap"));
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("Cannot apply this fix without confirmation")
    );
    assert!(!conflict.exists());

    let output = run_pager(
        &binary,
        &home,
        &echo_agent_home,
        "/bin/zsh",
        &["doctor", "fix", "ssh-wrap", "--yes"],
        &[("SSH_CONNECTION", "1 2 3 4")],
    );
    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("Run this fix on your local computer")
    );
    assert!(!conflict.exists());
}

#[cfg(unix)]
#[test]
#[ignore = "spawns the real pager binary; CI/Bazel provides PAGER_BINARY"]
fn restrictive_umask_still_preserves_exact_rc_mode() {
    use std::os::unix::fs::PermissionsExt as _;

    let binary = pager_binary().expect("real pager binary is required when selected");
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let echo_agent_home = temp.path().join("qhome");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&echo_agent_home).unwrap();
    let rc = home.join(".bashrc");
    std::fs::write(&rc, "export KEEP=1\n").unwrap();
    std::fs::set_permissions(&rc, std::fs::Permissions::from_mode(0o666)).unwrap();

    let mut command = base_pager_command(&binary, &home, &echo_agent_home, "/bin/bash");
    command.args(["doctor", "fix", "terminal.ssh-wrap", "--yes"]);
    use std::os::unix::process::CommandExt as _;
    // SAFETY: umask is async-signal-safe and runs only in the isolated child.
    unsafe {
        command.pre_exec(|| {
            libc::umask(0o077);
            Ok(())
        });
    }
    let output = command.output().expect("run restrictive-umask pager");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        std::fs::metadata(&rc).unwrap().permissions().mode() & 0o7777,
        0o666
    );
}

#[test]
#[ignore = "spawns the real pager binary; CI/Bazel provides PAGER_BINARY"]
fn wrap_non_tty_true_exec_preserves_argv_and_exit() {
    let binary = pager_binary().expect("real pager binary is required when selected");
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let echo_agent_home = temp.path().join("qhome");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&echo_agent_home).unwrap();
    let output = run_pager(
        &binary,
        &home,
        &echo_agent_home,
        "/bin/sh",
        &[
            "wrap",
            "/bin/sh",
            "-c",
            "printf '%s' \"$1\"; exit 7",
            "sh",
            "argv-ok",
        ],
        &[],
    );
    assert_eq!(output.status.code(), Some(7));
    assert_eq!(output.stdout, b"argv-ok");
}

fn run_pager(
    binary: &std::path::Path,
    home: &std::path::Path,
    echo_agent_home: &std::path::Path,
    shell: &str,
    args: &[&str],
    extra_env: &[(&str, &str)],
) -> std::process::Output {
    let mut command = base_pager_command(binary, home, echo_agent_home, shell);
    command.args(args).envs(extra_env.iter().copied());
    command.output().expect("run isolated pager binary")
}

fn base_pager_command(
    binary: &std::path::Path,
    home: &std::path::Path,
    echo_agent_home: &std::path::Path,
    shell: &str,
) -> Command {
    let mut command = Command::new(binary);
    command
        .env_clear()
        .env("HOME", home)
        .env("ECHO_AGENT_HOME", echo_agent_home)
        .env("SHELL", shell)
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("TERM", "xterm-256color")
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .envs(echo_agent_tty_utils::pager_env());
    echo_agent_tty_utils::detach_std_command(&mut command);
    command
}

fn directory_entries(path: &std::path::Path) -> HashSet<std::ffi::OsString> {
    std::fs::read_dir(path)
        .expect("read directory")
        .map(|entry| entry.expect("directory entry").file_name())
        .collect()
}
