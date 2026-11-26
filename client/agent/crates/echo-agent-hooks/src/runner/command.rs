// A panic on a teardown path leaks whatever it was about to free; tests panic freely.
#![cfg_attr(
    not(test),
    deny(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::todo,
        clippy::unimplemented
    )
)]

use std::borrow::Cow;
use std::sync::Arc;
use std::time::{Duration, Instant};

use echo_agent_tools::util::ProcessGroup;
use tokio::io::AsyncWriteExt;

use crate::config::{HookSpec, RUNNER_ALWAYS_SET_ENV};
use crate::event::HookEventEnvelope;
use crate::result::{HookDecision, StopHookOutcome};

use super::{
    GateHookJson, GateKind, HookRunnerResult, RunContext, StopHookJson, gate_json_to_decision,
    stop_json_to_outcome,
};

/// Maximum bytes to capture from hook stdout or stderr (64 KB).
const MAX_OUTPUT_BYTES: usize = 64 * 1024;

/// Exit code that a blocking hook uses to signal an explicit deny (PreToolUse)
/// or block (Stop/SubagentStop, with stderr as the feedback).
const GATE_EXIT_CODE: i32 = 2;

/// `None` when the group cannot be built, which only costs session reaping, so
/// the hook still runs.
fn hook_process_group(child: &tokio::process::Child) -> Option<Arc<ProcessGroup>> {
    let mut group = ProcessGroup::new()
        .inspect_err(
            |e| tracing::warn!(pid = child.id(), error = %e, "hook: no process group; not reaped on session close"),
        )
        .ok()?;
    group
        .attach(child)
        .inspect_err(
            |e| tracing::warn!(pid = child.id(), error = %e, "hook: process group attach failed; not reaped on session close"),
        )
        .ok()?;
    Some(Arc::new(group))
}

/// Run a single hook command.
///
/// Spawns the command as a child process, writes the envelope JSON on stdin,
/// reads stdout/stderr with buffer limits, enforces the timeout, and parses
/// the result.
pub async fn run_command_hook(
    spec: &HookSpec,
    envelope: &HookEventEnvelope,
    ctx: &RunContext<'_>,
    mode: GateKind,
) -> (HookRunnerResult, Duration) {
    let start = Instant::now();

    let Some(ref command) = spec.command else {
        return (
            HookRunnerResult::Failed("command hook has no 'command' field".into()),
            start.elapsed(),
        );
    };
    let command_str = command.to_string_lossy();

    let stdin_json = match serde_json::to_string(envelope) {
        Ok(j) => j,
        Err(e) => {
            let elapsed = start.elapsed();
            return (
                HookRunnerResult::Failed(format!("failed to serialize envelope: {e}")),
                elapsed,
            );
        }
    };

    let debug_payloads = std::env::var("ECHO_AGENT_HOOK_DEBUG").is_ok_and(|v| v == "1");
    if debug_payloads {
        tracing::trace!(
            hook_name = %spec.name,
            stdin_bytes = stdin_json.len(),
            "hook stdin payload"
        );
    }

    // Commands with shell metacharacters (spaces, pipes, &&, ||, redirects,
    // semicolons, env-var refs) or a leading `~` run through `sh -c` so shell
    // command strings from compatible configs work; everything else is a
    // direct executable path resolved from the hook file's directory.
    let is_shell_command = command_str.contains(' ')
        || command_str.contains('|')
        || command_str.contains('&')
        || command_str.contains(';')
        || command_str.contains('>')
        || command_str.contains('<')
        || command_str.contains('$')
        || command_str.starts_with('~');

    let mut cmd = if is_shell_command {
        // Fail fast on env vars we can't resolve (runner vars, per-hook
        // extra_env, or process env). Letting sh expand them to empty yields a
        // broken command that exits 127 with an opaque reason; surface a clear
        // error instead.
        let unresolved = find_unresolved_env_vars(&command_str, &spec.extra_env);
        if !unresolved.is_empty() {
            let elapsed = start.elapsed();
            let list = unresolved
                .iter()
                .map(|v| format!("${{{v}}}"))
                .collect::<Vec<_>>()
                .join(", ");
            return (
                HookRunnerResult::Failed(format!(
                    "hook not executed: required env var(s) not set: {list}"
                )),
                elapsed,
            );
        }
        #[cfg(unix)]
        {
            let mut c = tokio::process::Command::new("sh");
            c.arg("-c").arg(command_str.as_ref());
            c
        }
        #[cfg(not(unix))]
        {
            // PowerShell `$VAR` is not `$env:VAR`.
            let command_str = rewrite_hook_command_for_windows_shell(&command_str, &spec.extra_env);
            let inv = echo_agent_config::shell::shell_command_argv(command_str.as_ref());
            let mut c = tokio::process::Command::new(&inv.program);
            c.args(&inv.args).envs(inv.env);
            c
        }
    } else {
        let command_path = if command.is_absolute() {
            command.clone()
        } else {
            spec.source_dir.join(command)
        };
        if !command_path.exists() {
            let elapsed = start.elapsed();
            return (
                HookRunnerResult::Failed(format!("command not found: {}", command_path.display())),
                elapsed,
            );
        }
        tokio::process::Command::new(command_path)
    };

    // Detach from the controlling terminal so children (e.g. GPG pinentry)
    // can't open /dev/tty and corrupt the TUI display.
    echo_agent_tools::util::detach_command(&mut cmd);

    // Spawn the child process.
    //
    // SECURITY: env-var precedence at spawn time. `Command::envs(&map)` runs
    // AFTER any preceding `.env(...)` calls and silently overrides them, so
    // the order matters: we MUST apply user/plugin `extra_env` FIRST and
    // the runner-injected vars LAST. Otherwise a user JSON hook (or a
    // plugin) can spoof `ECHO_AGENT_HOOK_EVENT`, `ECHO_AGENT_HOOK_NAME`, `ECHO_AGENT_SESSION_ID`,
    // `ECHO_AGENT_WORKSPACE_ROOT`, or `CLAUDE_PROJECT_DIR`, which are the
    // identity/event signals a hook script consumes for policy and audit.
    // See the `runner_injected_vars_override_extra_env_at_spawn`
    // regression test in `tests/integration.rs` and the rustdoc on
    // `HookSpec::extra_env`.
    // Git Bash: `C:/...` so unquoted `$VAR` does not treat `\` as an escape.
    #[cfg(not(unix))]
    let env_root = {
        use echo_agent_config::shell::{WindowsShell, detect_windows_shell};
        if is_shell_command && matches!(detect_windows_shell(), WindowsShell::GitBash(_)) {
            Cow::Owned(ctx.workspace_root.replace('\\', "/"))
        } else {
            Cow::Borrowed(ctx.workspace_root)
        }
    };
    #[cfg(unix)]
    let env_root = Cow::Borrowed(ctx.workspace_root);

    #[allow(clippy::disallowed_methods)] // enrolled in the session scope below
    let mut child = match cmd
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .current_dir(ctx.workspace_root)
        // 1. user/plugin extra_env first (lowest precedence).
        .envs(&spec.extra_env)
        // 2. runner-injected vars last (highest precedence, always win).
        .env(
            "ECHO_AGENT_HOOK_EVENT",
            envelope.hook_event_name.to_string(),
        )
        .env("ECHO_AGENT_HOOK_NAME", &spec.name)
        .env("ECHO_AGENT_SESSION_ID", ctx.session_id)
        .env("ECHO_AGENT_WORKSPACE_ROOT", env_root.as_ref())
        // Compatibility alias for external hooks that read this env name.
        // Same value as `ECHO_AGENT_WORKSPACE_ROOT`; native `.echo-agent` hooks should use
        // `ECHO_AGENT_WORKSPACE_ROOT`.
        .env("CLAUDE_PROJECT_DIR", env_root.as_ref())
        .kill_on_drop(true)
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            let elapsed = start.elapsed();
            return (
                HookRunnerResult::Failed(format!("failed to spawn command: {e}")),
                elapsed,
            );
        }
    };

    let mut hook_group = None;
    if let Some(scope) = ctx.process_scope.as_ref()
        && let Some(group) = hook_process_group(&child)
    {
        // A closed scope means the session is gone and `register` already killed
        // the child, so stop rather than write stdin to a corpse.
        if !scope.register(&group) {
            return (
                HookRunnerResult::Failed("session closed before the hook ran".to_string()),
                start.elapsed(),
            );
        }
        hook_group = Some(group);
    }

    // Write stdin concurrently with draining output, under the timeout: a hook
    // that never reads stdin would otherwise block `write_all` on a full pipe
    // buffer, outside the deadline.
    let stdin = child.stdin.take();
    let timeout = Duration::from_millis(spec.timeout_ms);
    let result = tokio::time::timeout(timeout, async move {
        let write = async {
            if let Some(mut stdin) = stdin {
                let _ = stdin.write_all(stdin_json.as_bytes()).await;
            }
        };
        let (_, output) = tokio::join!(write, child.wait_with_output());
        output
    })
    .await;

    let elapsed = start.elapsed();

    // killpg takes grandchildren that kill_on_drop would miss.
    if !matches!(result, Ok(Ok(_)))
        && let Some(group) = &hook_group
    {
        let _ = group.kill();
    }

    match result {
        Err(_) => (
            HookRunnerResult::Failed(format!("timed out after {}ms", spec.timeout_ms)),
            elapsed,
        ),
        Ok(Err(e)) => (
            HookRunnerResult::Failed(format!("command execution failed: {e}")),
            elapsed,
        ),
        Ok(Ok(output)) => {
            let exit_code = output.status.code().unwrap_or(-1);

            let stdout = truncate_output(&output.stdout);
            let stderr = truncate_output(&output.stderr);

            if !stderr.is_empty() {
                // Byte counts always; the actual first line only for failing
                // runs (diagnosable from the log record alone) so successful
                // hooks don't write hook-authored text on every run.
                if exit_code != 0 {
                    tracing::debug!(
                        hook_name = %spec.name,
                        stderr_bytes = stderr.len(),
                        stderr_first_line = stderr_first_line(&stderr).unwrap_or_default(),
                        "hook stderr output captured"
                    );
                } else {
                    tracing::debug!(
                        hook_name = %spec.name,
                        stderr_bytes = stderr.len(),
                        "hook stderr output captured"
                    );
                }
            }

            if debug_payloads {
                tracing::trace!(
                    hook_name = %spec.name,
                    stdout_bytes = stdout.len(),
                    "hook stdout payload"
                );
            }

            tracing::debug!(
                hook_name = %spec.name,
                exit_code,
                stdout_bytes = stdout.len(),
                stderr_bytes = stderr.len(),
                elapsed_ms = elapsed.as_millis() as u64,
                "hook command completed"
            );

            match mode {
                GateKind::Observe => {
                    if exit_code == 0 {
                        return (HookRunnerResult::Success, elapsed);
                    }
                    (
                        HookRunnerResult::Failed(append_stderr_line(
                            &format!("exit code {exit_code}"),
                            &stderr,
                        )),
                        elapsed,
                    )
                }
                GateKind::Tool => {
                    parse_blocking_result(&stdout, &stderr, exit_code, &spec.name, elapsed)
                }
                GateKind::Stop => {
                    parse_stop_result(&stdout, &stderr, exit_code, &spec.name, elapsed)
                }
            }
        }
    }
}

#[cfg(any(test, not(unix)))]
#[derive(Clone, Copy, PartialEq, Eq)]
enum PsQuote {
    Bare,
    Single,
    Double,
}

#[cfg(any(test, not(unix)))]
fn rewrite_posix_env_refs_for_powershell<'a>(
    command: &'a str,
    extra_env: &std::collections::HashMap<String, String>,
) -> Cow<'a, str> {
    let mut out: Option<String> = None;
    let mut cursor = 0;
    let mut first_rewrite_at: Option<usize> = None;
    for r in crate::env_expand::iter_env_var_references(command) {
        if r.start < cursor {
            continue;
        }
        if r.name.is_empty() || r.has_modifier {
            continue;
        }
        if !RUNNER_ALWAYS_SET_ENV.contains(&r.name) && !extra_env.contains_key(r.name) {
            continue;
        }
        let (quote, escaped) = powershell_ctx_at(command, r.start);
        if quote == PsQuote::Single || escaped {
            continue;
        }
        let buf = out.get_or_insert_with(|| String::with_capacity(command.len() + 24));
        if quote == PsQuote::Bare {
            let token_end = command[r.start..]
                .find(|c: char| {
                    c.is_whitespace()
                        || matches!(c, ';' | '|' | '&' | '<' | '>' | '(' | ')' | '[' | ']' | ',')
                })
                .map_or(command.len(), |i| r.start + i);
            buf.push_str(&command[cursor..r.start]);
            buf.push('"');
            rewrite_ps_env_refs_in_span(buf, &command[r.start..token_end], extra_env);
            buf.push('"');
            cursor = token_end;
        } else {
            buf.push_str(&command[cursor..r.start]);
            push_ps_env_ref(buf, r.braced, r.name);
            cursor = r.end;
        }
        if first_rewrite_at.is_none() {
            first_rewrite_at = Some(r.start);
        }
    }
    match out {
        None => Cow::Borrowed(command),
        Some(mut buf) => {
            buf.push_str(&command[cursor..]);
            if first_rewrite_at.is_some_and(|at| {
                let pad = command.len() - command.trim_start().len();
                at == pad || (command.as_bytes().get(pad) == Some(&b'"') && at == pad + 1)
            }) && !buf.starts_with("& ")
            {
                buf.insert_str(0, "& ");
            }
            Cow::Owned(buf)
        }
    }
}

#[cfg(any(test, not(unix)))]
fn rewrite_ps_env_refs_in_span(
    buf: &mut String,
    span: &str,
    extra_env: &std::collections::HashMap<String, String>,
) {
    let mut cur = 0;
    for r in crate::env_expand::iter_env_var_references(span) {
        if r.name.is_empty() || r.has_modifier {
            continue;
        }
        if !RUNNER_ALWAYS_SET_ENV.contains(&r.name) && !extra_env.contains_key(r.name) {
            continue;
        }
        buf.push_str(&span[cur..r.start]);
        push_ps_env_ref(buf, r.braced, r.name);
        cur = r.end;
    }
    buf.push_str(&span[cur..]);
}

#[cfg(any(test, not(unix)))]
fn push_ps_env_ref(buf: &mut String, braced: bool, name: &str) {
    if braced {
        buf.push_str("${env:");
        buf.push_str(name);
        buf.push('}');
    } else {
        buf.push_str("$env:");
        buf.push_str(name);
    }
}

#[cfg(any(test, not(unix)))]
fn powershell_ctx_at(command: &str, at: usize) -> (PsQuote, bool) {
    let bytes = command.as_bytes();
    let mut i = 0;
    let mut quote = PsQuote::Bare;
    while i < at {
        let c = bytes[i];
        match quote {
            PsQuote::Single => {
                if c == b'\'' {
                    quote = PsQuote::Bare;
                }
                i += 1;
            }
            PsQuote::Double => {
                if c == b'`' {
                    i = i.saturating_add(2);
                } else if c == b'"' {
                    quote = PsQuote::Bare;
                    i += 1;
                } else {
                    i += 1;
                }
            }
            PsQuote::Bare => {
                if c == b'`' {
                    i = i.saturating_add(2);
                } else if c == b'\'' {
                    quote = PsQuote::Single;
                    i += 1;
                } else if c == b'"' {
                    quote = PsQuote::Double;
                    i += 1;
                } else {
                    i += 1;
                }
            }
        }
    }
    let escaped = quote != PsQuote::Single && at > 0 && bytes[at - 1] == b'`';
    (quote, escaped)
}

#[cfg(not(unix))]
fn rewrite_hook_command_for_windows_shell<'a>(
    command: &'a str,
    extra_env: &std::collections::HashMap<String, String>,
) -> Cow<'a, str> {
    use echo_agent_config::shell::{WindowsShell, detect_windows_shell};
    match detect_windows_shell() {
        WindowsShell::Pwsh | WindowsShell::PowerShell => {
            rewrite_posix_env_refs_for_powershell(command, extra_env)
        }
        WindowsShell::GitBash(_) => Cow::Borrowed(command),
        WindowsShell::Cmd => {
            if command.contains('$') {
                tracing::warn!(
                    "hook command uses $VAR but the Windows shell is cmd, which expands %VAR%"
                );
            }
            Cow::Borrowed(command)
        }
    }
}

/// Parse `command_str` for `${VAR}` and `$VAR` references and return the
/// names that aren't resolvable from any of:
///
/// * the runner's always-set env vars (see [`RUNNER_ALWAYS_SET_ENV`]),
/// * the per-hook `extra_env` map (set by the plugin adapter for plugin
///   hooks),
/// * the EchoAgent process's own environment (which is inherited by the child),
/// * local shell assignments inside the command itself (e.g. an
///   `INPUT=$(cat)` earlier in the string defines `INPUT` for the rest of
///   the command).
///
/// Names that appear inside a parameter-expansion form with a default,
/// fallback, or substitution modifier (`${VAR:-x}`, `${VAR-x}`, `${VAR:=x}`,
/// `${VAR:?msg}`, `${VAR:+x}`, `${VAR%pat}`, `${VAR#pat}`, `${VAR/pat/repl}`,
/// `${VAR:offset}`) are deliberately NOT flagged: the user has explicitly
/// handled the unset case in the shell expression, so the runner shouldn't
/// second-guess them.
///
/// The returned list is sorted and de-duplicated. Names are bare (no `$` or
/// `{}`).
fn find_unresolved_env_vars(
    command_str: &str,
    extra_env: &std::collections::HashMap<String, String>,
) -> Vec<String> {
    let locally_assigned = find_local_shell_assignments(command_str);
    let mut out: Vec<String> = Vec::new();
    for r in crate::env_expand::iter_env_var_references(command_str) {
        if r.name.is_empty() || r.has_modifier {
            continue;
        }
        if RUNNER_ALWAYS_SET_ENV.contains(&r.name) {
            continue;
        }
        if extra_env.contains_key(r.name) {
            continue;
        }
        if std::env::var_os(r.name).is_some() {
            continue;
        }
        if locally_assigned.contains(r.name) {
            continue;
        }
        out.push(r.name.to_string());
    }
    out.sort();
    out.dedup();
    out
}

/// Find shell variable assignments within `command_str` so that subsequent
/// `${VAR}` references to those names aren't flagged as undefined.
///
/// Detects two patterns common in inline hook commands:
///
/// * Plain assignments at the start of a command position: `VAR=value`,
///   `VAR=$(cmd)`, `VAR="..."`. The identifier must follow either the
///   start of the string, whitespace, or a statement separator (`;`, `&`,
///   `|`, `\n`).
/// * `read VAR1 VAR2 ...` statements (very common pattern for consuming
///   stdin in hooks).
///
/// This is a deliberately small heuristic, not a full shell parser. It
/// errs on the side of treating an identifier as locally set; the
/// consequence of a false negative here is a false positive in
/// [`find_unresolved_env_vars`] (which is precisely what we're trying to
/// avoid). Callers who need to be sure can always use the parameter-
/// expansion default form (`${VAR:-}`).
fn find_local_shell_assignments(command_str: &str) -> std::collections::HashSet<String> {
    let mut names = std::collections::HashSet::new();
    let bytes = command_str.as_bytes();
    let mut i = 0;
    let is_statement_start = |idx: usize| -> bool {
        if idx == 0 {
            return true;
        }
        let mut j = idx;
        while j > 0 {
            let c = bytes[j - 1];
            if c == b' ' || c == b'\t' {
                j -= 1;
                continue;
            }
            return matches!(c, b';' | b'&' | b'|' | b'\n' | b'(' | b'{');
        }
        true
    };
    while i < bytes.len() {
        let c = bytes[i];
        if !(c.is_ascii_alphabetic() || c == b'_') {
            i += 1;
            continue;
        }
        let start = i;
        while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
            i += 1;
        }
        let ident = std::str::from_utf8(&bytes[start..i]).unwrap_or("");
        if ident.is_empty() {
            continue;
        }
        if i < bytes.len() && bytes[i] == b'=' && is_statement_start(start) {
            names.insert(ident.to_string());
            continue;
        }
        if ident == "read" && is_statement_start(start) {
            while i < bytes.len() && (bytes[i] == b' ' || bytes[i] == b'\t') {
                i += 1;
            }
            while i < bytes.len() {
                let c2 = bytes[i];
                if matches!(c2, b';' | b'&' | b'|' | b'\n' | b'<' | b'>') {
                    break;
                }
                if c2 == b' ' || c2 == b'\t' {
                    i += 1;
                    continue;
                }
                if c2 == b'-' {
                    // `read -r VAR` etc.: skip the option flag.
                    while i < bytes.len() && bytes[i] != b' ' && bytes[i] != b'\t' {
                        i += 1;
                    }
                    continue;
                }
                if !(c2.is_ascii_alphabetic() || c2 == b'_') {
                    break;
                }
                let s = i;
                while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                    i += 1;
                }
                let read_ident = std::str::from_utf8(&bytes[s..i]).unwrap_or("");
                if !read_ident.is_empty() {
                    names.insert(read_ident.to_string());
                }
            }
        }
    }
    names
}

/// Cap for the stderr excerpt reused as deny reasons and failure detail:
/// long enough for a real policy message, short enough that one huge line
/// (capture allows up to 64 KB with no newline) cannot flood the model
/// message, scrollback, or exported logs. Cut on a char boundary with an
/// ellipsis, like the HTTP runner's response preview.
const MAX_STDERR_LINE_CHARS: usize = 256;

/// First non-empty stderr line, trimmed and capped at
/// [`MAX_STDERR_LINE_CHARS`]. A hook's stderr is its human feedback channel,
/// so failure results and exit-2 deny reasons surface this line instead of
/// only an exit code.
///
/// Deliberate shape difference vs `Stop` gates: deny reasons and failure
/// detail are one-line audit/UI strings, while a stop block's feedback is
/// model-facing instruction text and keeps the FULL trimmed stderr (see
/// [`parse_stop_result`]).
fn stderr_first_line(stderr: &str) -> Option<String> {
    let line = stderr.lines().map(str::trim).find(|l| !l.is_empty())?;
    if line.chars().count() <= MAX_STDERR_LINE_CHARS {
        return Some(line.to_string());
    }
    let mut capped: String = line.chars().take(MAX_STDERR_LINE_CHARS).collect();
    capped.push('\u{2026}');
    Some(capped)
}

/// Append the first (capped) stderr line to a failure message
/// (`"exit code 1: <line>"`), or return the message unchanged when stderr
/// is empty.
fn append_stderr_line(message: &str, stderr: &str) -> String {
    match stderr_first_line(stderr) {
        Some(line) => format!("{message}: {line}"),
        None => message.to_string(),
    }
}

/// Parse the result of a blocking hook from stdout, stderr, and exit code.
/// On exit 2 with no JSON reason, the first stderr line is the deny feedback;
/// non-gate exit codes carry it in the failure detail.
fn parse_blocking_result(
    stdout: &str,
    stderr: &str,
    exit_code: i32,
    hook_name: &str,
    elapsed: Duration,
) -> (HookRunnerResult, Duration) {
    let json_decision = if !stdout.trim().is_empty() {
        serde_json::from_str::<GateHookJson>(stdout.trim())
            .ok()
            .filter(GateHookJson::is_gate_document)
    } else {
        None
    };

    if let Some(output) = json_decision {
        match gate_json_to_decision(&output, hook_name, stderr_first_line(stderr).as_deref()) {
            Ok(HookDecision::Deny { reason, hook_name }) => {
                // A JSON deny is honored on any exit code (fail-safe).
                if exit_code != GATE_EXIT_CODE && exit_code != 0 {
                    tracing::warn!(
                        hook_name,
                        exit_code,
                        "JSON decision is 'deny' but exit code is not 0 or 2 — using JSON decision"
                    );
                }
                return (HookRunnerResult::Deny { reason, hook_name }, elapsed);
            }
            Ok(HookDecision::Allow) => {
                if exit_code == GATE_EXIT_CODE {
                    // Exit 2 wins over a JSON allow (stdout is not
                    // processed on exit 2); the exit-code ladder below
                    // denies.
                    tracing::warn!(
                        hook_name,
                        "JSON decision is 'allow' but exit code is 2 — denying (stdout is ignored on exit 2)"
                    );
                } else {
                    return (
                        HookRunnerResult::Allow {
                            updated_input: output.updated_input(hook_name),
                        },
                        elapsed,
                    );
                }
            }
            // Unknown decision value: failure so typos surface, carrying the
            // stderr line like every other failure on this path.
            Err(err) => {
                return (
                    HookRunnerResult::Failed(append_stderr_line(&err, stderr)),
                    elapsed,
                );
            }
        }
    }

    match exit_code {
        0 => (
            HookRunnerResult::Allow {
                updated_input: None,
            },
            elapsed,
        ),
        GATE_EXIT_CODE => (
            HookRunnerResult::Deny {
                // On exit 2 stderr is the deny feedback channel. First line
                // only: a deny reason is a one-line audit/UI string (stop
                // blocks keep full stderr — see `parse_stop_result`).
                reason: stderr_first_line(stderr).unwrap_or_else(|| {
                    format!("denied by hook '{hook_name}' (exit code {GATE_EXIT_CODE})")
                }),
                hook_name: hook_name.to_string(),
            },
            elapsed,
        ),
        _ => (
            HookRunnerResult::Failed(append_stderr_line(
                &format!("hook '{hook_name}' failed with exit code {exit_code}"),
                stderr,
            )),
            elapsed,
        ),
    }
}

/// Parse the result of a `Stop`/`SubagentStop` gate hook from stdout, stderr,
/// and exit code:
///
/// A valid decision JSON on stdout wins over the exit code. The exit code
/// decides only when stdout carries no usable JSON.
///
/// * **JSON stdout (any exit code)**: parsed as [`StopHookJson`]:
///   `decision: "block"` (+ `reason`), `continue: false` (+ `stopReason`), and
///   `hookSpecificOutput.additionalContext`.
/// * **no JSON + exit 0**: plain allow-stop.
/// * **no JSON + exit 2**: block, with stderr as the feedback fed to the model.
/// * **no JSON + any other exit code**: failure (callers fail open: the agent
///   stops normally).
fn parse_stop_result(
    stdout: &str,
    stderr: &str,
    exit_code: i32,
    hook_name: &str,
    elapsed: Duration,
) -> (HookRunnerResult, Duration) {
    let trimmed = stdout.trim();
    if !trimmed.is_empty() {
        match serde_json::from_str::<StopHookJson>(trimmed) {
            Ok(json) => {
                return match stop_json_to_outcome(json, hook_name) {
                    Ok(outcome) => (HookRunnerResult::Stop(outcome), elapsed),
                    Err(err) => (HookRunnerResult::Failed(err), elapsed),
                };
            }
            Err(err) => {
                // JSON-looking output that fails to parse is likely a broken
                // decision; warn and fall back to the exit code.
                if trimmed.starts_with('{') {
                    tracing::warn!(
                        hook_name,
                        error = %err,
                        "stop hook stdout looks like JSON but failed to parse; falling back to the exit code"
                    );
                }
            }
        }
    }
    match exit_code {
        0 => (HookRunnerResult::Stop(StopHookOutcome::default()), elapsed),
        GATE_EXIT_CODE => {
            // Full trimmed stderr on purpose: a stop block's feedback is
            // model-facing instruction text, often multi-line (deny reasons
            // keep one capped line — see `stderr_first_line`).
            let feedback = stderr.trim();
            let block_reason = if feedback.is_empty() {
                format!("Blocked by stop hook '{hook_name}' (exit code {GATE_EXIT_CODE})")
            } else {
                feedback.to_string()
            };
            (
                HookRunnerResult::Stop(StopHookOutcome {
                    block_reason: Some(block_reason),
                    ..Default::default()
                }),
                elapsed,
            )
        }
        _ => (
            HookRunnerResult::Failed(append_stderr_line(
                &format!("hook '{hook_name}' failed with exit code {exit_code}"),
                stderr,
            )),
            elapsed,
        ),
    }
}

/// Truncate output bytes to MAX_OUTPUT_BYTES and convert to a lossy UTF-8 string.
fn truncate_output(bytes: &[u8]) -> String {
    if bytes.len() <= MAX_OUTPUT_BYTES {
        String::from_utf8_lossy(bytes).into_owned()
    } else {
        let mut truncated = String::from_utf8_lossy(&bytes[..MAX_OUTPUT_BYTES]).into_owned();
        truncated.push_str(" [truncated]");
        tracing::warn!(
            total_bytes = bytes.len(),
            max_bytes = MAX_OUTPUT_BYTES,
            "hook output truncated"
        );
        truncated
    }
}

/// Resolve the absolute command path for a hook spec.
///
/// Returns `None` for non-command handler types.
pub fn resolve_command_path(spec: &HookSpec) -> Option<std::path::PathBuf> {
    let command = spec.command.as_ref()?;
    if command.is_absolute() {
        Some(command.clone())
    } else {
        Some(spec.source_dir.join(command))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_json_decision() {
        let (allow, _) =
            parse_blocking_result(r#"{"decision":"allow"}"#, "", 0, "test", Duration::ZERO);
        assert!(matches!(allow, HookRunnerResult::Allow { .. }));

        let (deny, _) = parse_blocking_result(
            r#"{"decision":"deny","reason":"bad command"}"#,
            "",
            2,
            "test",
            Duration::ZERO,
        );
        match deny {
            HookRunnerResult::Deny { reason, .. } => {
                assert_eq!(reason, "bad command");
            }
            other => panic!("expected Deny, got {other:?}"),
        }

        let (deny_no_reason, _) =
            parse_blocking_result(r#"{"decision":"deny"}"#, "", 2, "my-hook", Duration::ZERO);
        match deny_no_reason {
            HookRunnerResult::Deny { reason, .. } => {
                assert!(reason.contains("my-hook"));
            }
            other => panic!("expected Deny, got {other:?}"),
        }

        let (unknown, _) =
            parse_blocking_result(r#"{"decision":"maybe"}"#, "", 0, "test", Duration::ZERO);
        assert!(matches!(unknown, HookRunnerResult::Failed(_)));
    }

    #[test]
    fn parse_updated_input() {
        let (allow, _) = parse_blocking_result(
            r#"{"hookSpecificOutput":{"updatedInput":{"command":"echo hi"}}}"#,
            "",
            0,
            "test",
            Duration::ZERO,
        );
        match allow {
            HookRunnerResult::Allow {
                updated_input: Some(input),
            } => assert_eq!(input["command"], "echo hi"),
            other => panic!("expected Allow with updatedInput, got {other:?}"),
        }

        let (deny, _) = parse_blocking_result(
            r#"{"decision":"deny","hookSpecificOutput":{"updatedInput":{"command":"x"}}}"#,
            "",
            0,
            "test",
            Duration::ZERO,
        );
        assert!(matches!(deny, HookRunnerResult::Deny { .. }));

        let (allow_no_rewrite, _) = parse_blocking_result(
            r#"{"hookSpecificOutput":{"updatedInput":"nope"}}"#,
            "",
            0,
            "test",
            Duration::ZERO,
        );
        assert!(matches!(
            allow_no_rewrite,
            HookRunnerResult::Allow {
                updated_input: None
            }
        ));
    }

    #[test]
    fn fallback_to_exit_code() {
        for (stdout, code, expect_allow) in
            [("", 0, true), ("not json at all", 0, true), ("", 2, false)]
        {
            let (result, _) = parse_blocking_result(stdout, "", code, "test", Duration::ZERO);
            if expect_allow {
                assert!(matches!(result, HookRunnerResult::Allow { .. }));
            } else {
                assert!(matches!(result, HookRunnerResult::Deny { .. }));
            }
        }
        let (fail, _) = parse_blocking_result("", "", 1, "test", Duration::ZERO);
        assert!(matches!(fail, HookRunnerResult::Failed(_)));
    }

    #[test]
    fn non_gate_json_falls_through_to_exit_code() {
        let (fail, _) =
            parse_blocking_result(r#"{"detail":"not found"}"#, "", 1, "test", Duration::ZERO);
        assert!(matches!(fail, HookRunnerResult::Failed(_)));

        let (allow, _) = parse_blocking_result(r#"{"detail":"ok"}"#, "", 0, "test", Duration::ZERO);
        assert!(matches!(allow, HookRunnerResult::Allow { .. }));
    }

    /// Failure results and exit-2 deny reasons carry the hook's first stderr
    /// line (stderr is the hook's feedback channel), instead of only an
    /// exit code.
    #[test]
    fn blocking_result_surfaces_stderr() {
        let deny_reason = |result: HookRunnerResult| match result {
            HookRunnerResult::Deny { reason, .. } => reason,
            other => panic!("expected Deny, got {other:?}"),
        };

        let (deny, _) = parse_blocking_result(
            "",
            "  \nrejected by policy\nmore\n",
            2,
            "test",
            Duration::ZERO,
        );
        assert_eq!(deny_reason(deny), "rejected by policy");

        let (fail, _) = parse_blocking_result("", "config missing\n", 1, "test", Duration::ZERO);
        match fail {
            HookRunnerResult::Failed(error) => assert!(
                error.contains("exit code 1") && error.contains("config missing"),
                "failure must carry exit code AND stderr text, got: {error}"
            ),
            other => panic!("expected Failed, got {other:?}"),
        }

        // A JSON deny without a usable reason also falls back to stderr.
        let (json_deny, _) = parse_blocking_result(
            r#"{"decision":"deny","reason":"  "}"#,
            "quota exceeded\n",
            0,
            "test",
            Duration::ZERO,
        );
        assert_eq!(deny_reason(json_deny), "quota exceeded");
    }

    /// One huge stderr line (capture allows 64 KB with no newline) must not
    /// become the whole deny reason: the excerpt is capped on a char boundary
    /// with an ellipsis, and multibyte chars survive the cut.
    #[test]
    fn stderr_line_is_capped() {
        let long = "é".repeat(MAX_STDERR_LINE_CHARS + 50);
        let capped = stderr_first_line(&long).expect("non-empty line");
        assert_eq!(capped.chars().count(), MAX_STDERR_LINE_CHARS + 1);
        assert!(capped.ends_with('\u{2026}'));

        let (deny, _) = parse_blocking_result("", &long, 2, "test", Duration::ZERO);
        match deny {
            HookRunnerResult::Deny { reason, .. } => {
                assert!(reason.chars().count() <= MAX_STDERR_LINE_CHARS + 1);
            }
            other => panic!("expected Deny, got {other:?}"),
        }

        // At the cap: no ellipsis, nothing lost.
        let exact = "x".repeat(MAX_STDERR_LINE_CHARS);
        assert_eq!(stderr_first_line(&exact).as_deref(), Some(exact.as_str()));
    }

    /// A blank JSON `reason` is not a reason: command hooks fall back to the
    /// stderr line, and with no fallback (the HTTP handler has no stderr
    /// channel) the generic deny message is used — never the blank string.
    #[test]
    fn blank_json_reason_falls_back() {
        let blank = || GateHookJson {
            decision: Some("deny".to_string()),
            reason: Some("  ".to_string()),
            hook_specific_output: None,
        };
        let with_fallback =
            gate_json_to_decision(&blank(), "h", Some("quota exceeded")).expect("valid decision");
        assert!(
            matches!(with_fallback, HookDecision::Deny { ref reason, .. } if reason == "quota exceeded")
        );

        let without_fallback = gate_json_to_decision(&blank(), "h", None).expect("valid decision");
        assert!(
            matches!(without_fallback, HookDecision::Deny { ref reason, .. } if reason == "denied by hook 'h'")
        );
    }

    /// Unknown JSON decision values fail with the stderr line attached, like
    /// every other failure on the gate path.
    #[test]
    fn unknown_decision_failure_carries_stderr() {
        let (result, _) = parse_blocking_result(
            r#"{"decision":"maybe"}"#,
            "config missing\n",
            1,
            "test",
            Duration::ZERO,
        );
        match result {
            HookRunnerResult::Failed(error) => assert!(
                error.contains("maybe") && error.contains("config missing"),
                "got: {error}"
            ),
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    /// The observe path reports `exit code N: <first stderr line>` so the
    /// scrollback and log record are diagnosable without hunting for output.
    /// Unix-only like the sibling real-process tests: the script relies on
    /// POSIX `sh` semantics (`>&2`).
    #[tokio::test]
    #[cfg(unix)]
    async fn observe_failure_carries_stderr_line() {
        let spec = make_shell_spec("echo 'disk full' >&2; exit 1");
        let (result, _) =
            run_command_hook(&spec, &make_envelope(), &make_ctx(), GateKind::Observe).await;
        match result {
            HookRunnerResult::Failed(error) => assert_eq!(error, "exit code 1: disk full"),
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[test]
    fn json_decision_vs_exit_code() {
        let (deny, _) = parse_blocking_result(
            r#"{"decision":"deny","reason":"nope"}"#,
            "",
            0,
            "test",
            Duration::ZERO,
        );
        assert!(matches!(deny, HookRunnerResult::Deny { .. }));

        let (blocked, _) =
            parse_blocking_result(r#"{"decision":"allow"}"#, "", 2, "test", Duration::ZERO);
        assert!(matches!(blocked, HookRunnerResult::Deny { .. }));
    }

    fn stop_outcome(result: HookRunnerResult) -> StopHookOutcome {
        match result {
            HookRunnerResult::Stop(outcome) => outcome,
            other => panic!("expected Stop outcome, got {other:?}"),
        }
    }

    #[test]
    fn stop_block_decision_with_reason() {
        let (result, _) = parse_stop_result(
            r#"{"decision":"block","reason":"tests are failing"}"#,
            "",
            0,
            "my-stop",
            Duration::ZERO,
        );
        let outcome = stop_outcome(result);
        assert_eq!(
            outcome,
            StopHookOutcome {
                block_reason: Some("tests are failing".into()),
                ..Default::default()
            }
        );

        let (result, _) =
            parse_stop_result(r#"{"decision":"block"}"#, "", 0, "my-stop", Duration::ZERO);
        assert_eq!(
            stop_outcome(result).block_reason.as_deref(),
            Some("Blocked by stop hook 'my-stop'")
        );
    }

    #[test]
    fn stop_exit_2_blocks_with_stderr() {
        let (result, _) =
            parse_stop_result("", "run the test suite first\n", 2, "s", Duration::ZERO);
        assert_eq!(
            stop_outcome(result).block_reason.as_deref(),
            Some("run the test suite first")
        );

        let (result, _) = parse_stop_result("", "", 2, "s", Duration::ZERO);
        assert_eq!(
            stop_outcome(result).block_reason.as_deref(),
            Some("Blocked by stop hook 's' (exit code 2)")
        );
    }

    #[test]
    fn stop_stdout_json_wins_over_exit_2() {
        let (result, _) = parse_stop_result(
            r#"{"continue":false,"stopReason":"enough","hookSpecificOutput":{"additionalContext":"ctx"}}"#,
            "log noise\n",
            2,
            "s",
            Duration::ZERO,
        );
        let outcome = stop_outcome(result);
        assert_eq!(
            outcome
                .force_stop
                .as_ref()
                .and_then(|f| f.reason.as_deref()),
            Some("enough")
        );
        assert_eq!(outcome.additional_context.as_deref(), Some("ctx"));

        let (result, _) = parse_stop_result("log noise\n", "blocked", 2, "s", Duration::ZERO);
        assert_eq!(
            stop_outcome(result).block_reason.as_deref(),
            Some("blocked")
        );
    }

    #[test]
    fn stop_continue_false_prevents_continuation() {
        let (result, _) = parse_stop_result(
            r#"{"continue":false,"stopReason":"budget exhausted"}"#,
            "",
            0,
            "s",
            Duration::ZERO,
        );
        let outcome = stop_outcome(result);
        assert_eq!(
            outcome,
            StopHookOutcome {
                force_stop: Some(crate::result::StopOverride {
                    reason: Some("budget exhausted".into()),
                }),
                ..Default::default()
            }
        );
        let (result, _) = parse_stop_result(r#"{"continue":true}"#, "", 0, "s", Duration::ZERO);
        assert!(stop_outcome(result).is_empty());
    }

    #[test]
    fn stop_additional_context_captured() {
        let (result, _) = parse_stop_result(
            r#"{"hookSpecificOutput":{"hookEventName":"Stop","additionalContext":"run the test suite before finishing"}}"#,
            "",
            0,
            "s",
            Duration::ZERO,
        );
        let outcome = stop_outcome(result);
        assert_eq!(
            outcome,
            StopHookOutcome {
                additional_context: Some("run the test suite before finishing".into()),
                ..Default::default()
            }
        );
    }

    #[test]
    fn stop_allow_failure_and_unknown_decision() {
        let (result, _) = parse_stop_result("", "", 0, "s", Duration::ZERO);
        assert!(stop_outcome(result).is_empty());

        let (result, _) = parse_stop_result("all done!", "", 0, "s", Duration::ZERO);
        assert!(stop_outcome(result).is_empty());

        let (result, _) = parse_stop_result("", "boom", 1, "s", Duration::ZERO);
        match result {
            HookRunnerResult::Failed(error) => assert!(
                error.contains("exit code 1") && error.contains("boom"),
                "stop failure must carry exit code AND stderr text, got: {error}"
            ),
            other => panic!("expected Failed, got {other:?}"),
        }

        let (result, _) = parse_stop_result(r#"{"decision":"deny"}"#, "", 0, "s", Duration::ZERO);
        assert!(matches!(result, HookRunnerResult::Failed(_)));

        // `approve` is accepted as a no-op (shared approve/block vocabulary).
        let (result, _) =
            parse_stop_result(r#"{"decision":"approve"}"#, "", 0, "s", Duration::ZERO);
        assert!(stop_outcome(result).is_empty());
    }

    #[test]
    fn stop_output_captures_all_combined_signals() {
        let (result, _) = parse_stop_result(
            r#"{"decision":"block","reason":"keep going","continue":false,"stopReason":"user said stop","hookSpecificOutput":{"additionalContext":"ctx"}}"#,
            "",
            0,
            "s",
            Duration::ZERO,
        );
        let outcome = stop_outcome(result);
        assert_eq!(
            outcome,
            StopHookOutcome {
                block_reason: Some("keep going".into()),
                additional_context: Some("ctx".into()),
                force_stop: Some(crate::result::StopOverride {
                    reason: Some("user said stop".into()),
                }),
            }
        );
    }

    #[test]
    fn truncate_output_respects_limit() {
        assert_eq!(truncate_output(b"hello world"), "hello world");

        let large = truncate_output(&vec![b'x'; MAX_OUTPUT_BYTES + 1000]);
        assert!(large.ends_with(" [truncated]"));
    }

    #[test]
    fn resolve_command_path_variants() {
        let spec =
            |handler: crate::config::HandlerType, command: Option<&str>, source: &str| HookSpec {
                name: "test".into(),
                event: crate::event::HookEventName::PreToolUse,
                handler_type: handler,
                configured_matcher: None,
                matcher: None,
                enabled: true,
                command: command.map(std::path::PathBuf::from),
                command_raw: command.map(str::to_string),
                url: None,
                url_raw: None,
                timeout_ms: 5000,
                source_dir: std::path::PathBuf::from(source),
                extra_env: std::collections::HashMap::new(),
                layer: crate::config::HookProvenance::File,
            };
        use crate::config::HandlerType;
        assert_eq!(
            resolve_command_path(&spec(
                HandlerType::Command,
                Some("/usr/bin/hook"),
                "/some/dir"
            )),
            Some(std::path::PathBuf::from("/usr/bin/hook"))
        );
        assert_eq!(
            resolve_command_path(&spec(
                HandlerType::Command,
                Some("bin/check.sh"),
                "/project/.echo-agent/hooks"
            )),
            Some(std::path::PathBuf::from(
                "/project/.echo-agent/hooks/bin/check.sh"
            ))
        );
        assert_eq!(
            resolve_command_path(&spec(HandlerType::Http, None, "/project")),
            None
        );
    }

    /// Helper to build a HookSpec that runs a shell command.
    fn make_shell_spec(command: &str) -> HookSpec {
        HookSpec {
            name: "test-hook".into(),
            event: crate::event::HookEventName::Stop,
            handler_type: crate::config::HandlerType::Command,
            configured_matcher: None,
            matcher: None,
            enabled: true,
            command: Some(command.into()),
            command_raw: Some(command.to_string()),
            url: None,
            url_raw: None,
            timeout_ms: 5000,
            source_dir: std::env::temp_dir(),
            extra_env: std::collections::HashMap::new(),
            layer: crate::config::HookProvenance::File,
        }
    }

    fn make_envelope() -> HookEventEnvelope {
        use crate::event::HookPayload;
        HookEventEnvelope {
            hook_event_name: crate::event::HookEventName::Stop,
            session_id: "test-session".into(),
            cwd: "/tmp".into(),
            workspace_root: "/tmp".into(),
            timestamp: "2026-01-01T00:00:00Z".into(),
            transcript_path: None,
            client_identifier: None,
            prompt_id: None,
            permission_mode: None,
            payload: HookPayload::Stop {
                reason: "test".into(),
                stop_hook_active: false,
                last_assistant_message: None,
                background_tasks: None,
                session_crons: None,
            },
        }
    }

    fn make_ctx() -> RunContext<'static> {
        RunContext {
            session_id: "test-session",
            workspace_root: "/tmp",
            process_scope: None,
        }
    }

    fn make_scoped_ctx(scope: echo_agent_tools::util::ProcessScope) -> RunContext<'static> {
        RunContext {
            process_scope: Some(scope),
            ..make_ctx()
        }
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn hook_times_out() {
        let mut spec = make_shell_spec("sleep 5");
        spec.timeout_ms = 100;
        let envelope = make_envelope();
        let ctx = make_ctx();
        let (result, _) = run_command_hook(&spec, &envelope, &ctx, GateKind::Observe).await;
        assert!(
            matches!(&result, HookRunnerResult::Failed(msg) if msg.contains("timed out")),
            "expected a timeout failure, got {result:?}"
        );
    }

    /// Regression: a hook that never reads stdin while writing large stdout must
    /// not deadlock, since stdin is written concurrently with draining output.
    #[tokio::test]
    #[cfg(unix)]
    async fn large_envelope_with_unreading_hook_does_not_deadlock() {
        use crate::event::HookPayload;
        let spec = make_shell_spec("head -c 200000 /dev/zero | tr '\\0' x");
        let mut envelope = make_envelope();
        envelope.payload = HookPayload::Stop {
            reason: "test".into(),
            stop_hook_active: false,
            // Larger than the OS pipe buffer (~64 KB) so the stdin write blocks
            // without concurrent draining.
            last_assistant_message: Some("x".repeat(256 * 1024)),
            background_tasks: None,
            session_crons: None,
        };
        let ctx = make_ctx();
        let run = run_command_hook(&spec, &envelope, &ctx, GateKind::Observe);
        let (result, _) = tokio::time::timeout(std::time::Duration::from_secs(10), run)
            .await
            .expect("hook must not deadlock on a large envelope");
        assert!(matches!(result, HookRunnerResult::Success));
    }

    /// Verify that setsid() prevents hook child processes from opening
    /// `/dev/tty`. This is the core fix for GPG pinentry corruption.
    ///
    /// The hook tries `exec 3>/dev/tty` — if detached, this fails and the
    /// shell exits 1 (caught by `||`), making the overall command exit 0.
    /// If NOT detached, the open succeeds and the command exits 1.
    #[tokio::test]
    #[cfg(unix)]
    async fn test_hook_child_cannot_open_dev_tty() {
        // Skip in CI / environments without a controlling terminal —
        // setsid() gets EPERM when already a session leader and the
        // setpgid fallback doesn't detach /dev/tty.
        if std::fs::OpenOptions::new()
            .write(true)
            .open("/dev/tty")
            .is_err()
        {
            eprintln!("skipping: no controlling terminal");
            return;
        }

        // exit 0 if /dev/tty is inaccessible (DETACHED), exit 1 if accessible
        let spec = make_shell_spec("exec 3>/dev/tty 2>/dev/null && exit 1 || exit 0");
        let envelope = make_envelope();
        let ctx = make_ctx();

        let (result, _duration) = run_command_hook(&spec, &envelope, &ctx, GateKind::Observe).await;

        assert!(
            matches!(result, HookRunnerResult::Success),
            "hook child should not be able to open /dev/tty after setsid(), got {:?}",
            result
        );
    }

    #[tokio::test]
    async fn test_hook_blocking_allow() {
        let spec = make_shell_spec(r#"echo '{"decision":"allow"}'"#);
        let envelope = make_envelope();
        let ctx = make_ctx();

        let (result, _duration) = run_command_hook(&spec, &envelope, &ctx, GateKind::Tool).await;

        assert!(
            matches!(result, HookRunnerResult::Allow { .. }),
            "blocking hook should return Allow, got {:?}",
            result
        );
    }

    /// Regression: a hook command that uses `${VAR}` interpolation
    /// without any other shell metacharacters must still be invoked via
    /// `sh -c` so that the env var supplied via `extra_env` is expanded.
    /// Previously the runner treated `${...}` as part of a literal path
    /// and `command_path.exists()` failed; the hook silently never ran.
    /// Now the env-var pre-spawn check refuses with a clear reason when
    /// the var is unset (and the dispatcher fail-opens, so the tool call
    /// itself is not blocked).
    #[tokio::test]
    async fn test_env_var_interpolation_runs_via_shell() {
        let tmp = tempfile::tempdir().unwrap();
        let script = tmp.path().join("hook.sh");
        std::fs::write(&script, "#!/bin/sh\nexit 0\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&script).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&script, perms).unwrap();
        }

        let mut extra_env = std::collections::HashMap::new();
        extra_env.insert(
            "GB1183_PLUGIN_ROOT".to_string(),
            tmp.path().to_string_lossy().into_owned(),
        );

        let spec = HookSpec {
            name: "test-env-interp".into(),
            event: crate::event::HookEventName::Stop,
            handler_type: crate::config::HandlerType::Command,
            configured_matcher: None,
            matcher: None,
            enabled: true,
            command: Some(std::path::PathBuf::from("${GB1183_PLUGIN_ROOT}/hook.sh")),
            command_raw: Some("${GB1183_PLUGIN_ROOT}/hook.sh".to_string()),
            url: None,
            url_raw: None,
            timeout_ms: 5000,
            source_dir: tmp.path().to_path_buf(),
            extra_env,
            layer: crate::config::HookProvenance::File,
        };

        let envelope = make_envelope();
        let ctx = make_ctx();
        let (result, _) = run_command_hook(&spec, &envelope, &ctx, GateKind::Observe).await;

        assert!(
            matches!(result, HookRunnerResult::Success),
            "hook with ${{VAR}} interpolation should be expanded via sh -c, got {:?}",
            result
        );
    }

    /// `CLAUDE_PROJECT_DIR` is part of the external hook contract: it points
    /// to the workspace/project root and is set for ALL hooks (not just
    /// plugin-scoped ones). Plugin hooks frequently reference it as
    /// `"$CLAUDE_PROJECT_DIR/.claude/hooks/foo.sh"`. The runner must export
    /// it on the spawned child so shell expansion via the `sh -c` branch
    /// resolves correctly; otherwise such hooks fail to find the
    /// command.
    #[tokio::test]
    async fn test_claude_project_dir_is_exported() {
        let tmp = tempfile::tempdir().unwrap();
        let script = tmp.path().join("hook.sh");
        // Exit 0 only if CLAUDE_PROJECT_DIR matches the workspace root.
        let workspace = tmp.path().to_string_lossy().into_owned();
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\ntest \"${{CLAUDE_PROJECT_DIR}}\" = \"{workspace}\"\n",
                workspace = workspace
            ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&script).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&script, perms).unwrap();
        }

        let spec = HookSpec {
            name: "test-claude-project-dir".into(),
            event: crate::event::HookEventName::Stop,
            handler_type: crate::config::HandlerType::Command,
            configured_matcher: None,
            matcher: None,
            enabled: true,
            // Use ${CLAUDE_PROJECT_DIR} in the path itself so this also exercises
            // the `$` -> sh -c routing.
            command: Some(std::path::PathBuf::from("${CLAUDE_PROJECT_DIR}/hook.sh")),
            command_raw: Some("${CLAUDE_PROJECT_DIR}/hook.sh".to_string()),
            url: None,
            url_raw: None,
            timeout_ms: 5000,
            source_dir: tmp.path().to_path_buf(),
            extra_env: std::collections::HashMap::new(),
            layer: crate::config::HookProvenance::File,
        };

        let envelope = make_envelope();
        let ctx = RunContext {
            session_id: "test-session",
            workspace_root: &workspace,
            process_scope: None,
        };
        let (result, _) = run_command_hook(&spec, &envelope, &ctx, GateKind::Observe).await;

        assert!(
            matches!(result, HookRunnerResult::Success),
            "hook should see CLAUDE_PROJECT_DIR set to the workspace root, got {:?}",
            result
        );
    }

    #[test]
    fn powershell_rewrite_cases() {
        let mut extra = std::collections::HashMap::new();
        extra.insert("PLUGIN_ROOT".to_string(), "/unused".to_string());
        let cases = [
            (
                r#"powershell -File "$CLAUDE_PROJECT_DIR/.claude/hooks/foo.ps1" ${PLUGIN_ROOT}"#,
                r#"powershell -File "$env:CLAUDE_PROJECT_DIR/.claude/hooks/foo.ps1" "${env:PLUGIN_ROOT}""#,
            ),
            (
                r#"$UNKNOWN ${CLAUDE_PROJECT_DIR:-.}/x bash -c '$CLAUDE_PROJECT_DIR/x.sh' `$CLAUDE_PROJECT_DIR"#,
                r#"$UNKNOWN ${CLAUDE_PROJECT_DIR:-.}/x bash -c '$CLAUDE_PROJECT_DIR/x.sh' `$CLAUDE_PROJECT_DIR"#,
            ),
            (
                "$CLAUDE_PROJECT_DIR/.claude/hooks/foo.ps1",
                r#"& "$env:CLAUDE_PROJECT_DIR/.claude/hooks/foo.ps1""#,
            ),
            (
                "$CLAUDE_PROJECT_DIR/.claude/hooks/foo.ps1; echo done",
                r#"& "$env:CLAUDE_PROJECT_DIR/.claude/hooks/foo.ps1"; echo done"#,
            ),
            (
                r#""$CLAUDE_PROJECT_DIR/.claude/hooks/foo.ps1""#,
                r#"& "$env:CLAUDE_PROJECT_DIR/.claude/hooks/foo.ps1""#,
            ),
            (
                r#"powershell -File $CLAUDE_PROJECT_DIR/.claude/hooks/foo.ps1"#,
                r#"powershell -File "$env:CLAUDE_PROJECT_DIR/.claude/hooks/foo.ps1""#,
            ),
            (
                "$CLAUDE_PROJECT_DIR/$ECHO_AGENT_HOOK_NAME.ps1",
                r#"& "$env:CLAUDE_PROJECT_DIR/$env:ECHO_AGENT_HOOK_NAME.ps1""#,
            ),
            (
                "Join-Path ($CLAUDE_PROJECT_DIR) hooks",
                r#"Join-Path ("$env:CLAUDE_PROJECT_DIR") hooks"#,
            ),
            (
                r#"Write-Host "don't skip $CLAUDE_PROJECT_DIR""#,
                r#"Write-Host "don't skip $env:CLAUDE_PROJECT_DIR""#,
            ),
        ];
        for (input, want) in cases {
            assert_eq!(
                rewrite_posix_env_refs_for_powershell(input, &extra).as_ref(),
                want,
                "{input}"
            );
        }
    }

    /// `extra_env` seeds what's "set" so the test does not depend on the
    /// process environment.
    #[test]
    fn find_unresolved_detects_and_dedups() {
        let mut env = std::collections::HashMap::new();
        env.insert("KNOWN".to_string(), "x".to_string());
        assert_eq!(
            find_unresolved_env_vars("${KNOWN}/${SOME_GB1183_UNSET_VAR}/foo", &env),
            vec!["SOME_GB1183_UNSET_VAR".to_string()]
        );
        assert_eq!(
            find_unresolved_env_vars("$SOME_GB1183_BARE_UNSET/foo", &env),
            vec!["SOME_GB1183_BARE_UNSET".to_string()]
        );
        assert_eq!(
            find_unresolved_env_vars(
                "${MISSING_GB1183_DUP} && ${MISSING_GB1183_DUP}/foo $MISSING_GB1183_DUP",
                &env,
            ),
            vec!["MISSING_GB1183_DUP".to_string()]
        );
    }

    #[test]
    fn find_unresolved_skips_resolvable_vars() {
        let mut env = std::collections::HashMap::new();
        env.insert("CLAUDE_PLUGIN_ROOT".to_string(), "/plugins/foo".to_string());
        let v = find_unresolved_env_vars(
            "${ECHO_AGENT_HOOK_EVENT}/${CLAUDE_PROJECT_DIR}/${ECHO_AGENT_SESSION_ID}/${CLAUDE_PLUGIN_ROOT}/foo",
            &env,
        );
        assert!(
            v.is_empty(),
            "resolvable vars should not be flagged, got {v:?}"
        );
    }

    #[test]
    fn find_unresolved_skips_non_var_dollars() {
        let env = std::collections::HashMap::new();
        // $1 (positional), $$ (pid), $(...) (cmd subst), $? (exit code), $#.
        let v = find_unresolved_env_vars("echo $1 $$ $? $# $(date)", &env);
        assert!(
            v.is_empty(),
            "shell special params should not be flagged, got {v:?}"
        );
    }

    #[test]
    fn find_unresolved_skips_local_assignments() {
        let env = std::collections::HashMap::new();
        for cmd in [
            r#"INPUT=$(cat); echo "$INPUT" | grep -q foo"#,
            "read -r LINE; echo $LINE",
            "echo first; X=hello && echo $X | cat",
        ] {
            let v = find_unresolved_env_vars(cmd, &env);
            assert!(v.is_empty(), "`{cmd}` should not flag any var, got {v:?}");
        }
    }

    #[test]
    fn find_unresolved_skips_parameter_expansion_modifiers() {
        let env = std::collections::HashMap::new();
        // All of these explicitly handle the unset case; the runner must
        // not flag them, otherwise we reject hooks that the user wrote
        // correctly.
        let cases = [
            "${MISSING_GB1183_MOD:-/default/path.sh}",
            "${MISSING_GB1183_MOD-/default/path.sh}",
            "${MISSING_GB1183_MOD:=/assigned/path.sh}",
            "${MISSING_GB1183_MOD:?msg here}",
            "${MISSING_GB1183_MOD:+/used/if/set.sh}",
            "${MISSING_GB1183_MOD%.sh}",
            "${MISSING_GB1183_MOD#prefix/}",
            "${MISSING_GB1183_MOD/foo/bar}",
            "${MISSING_GB1183_MOD:0:5}",
        ];
        for case in cases {
            let v = find_unresolved_env_vars(case, &env);
            assert!(
                v.is_empty(),
                "parameter-expansion form `{case}` should not be flagged, got {v:?}"
            );
        }
    }

    /// Regression follow-up: when a hook command references
    /// an env var that isn't set anywhere we know about, the runner must
    /// refuse to spawn entirely (no fork+exec, no opaque "exit code 127")
    /// and surface a clear failure reason naming the missing var(s).
    #[tokio::test]
    async fn test_undefined_env_var_refuses_to_spawn() {
        let mut extra_env = std::collections::HashMap::new();
        // Intentionally do NOT set NEVER_SET_GB1183 anywhere.
        extra_env.insert("UNRELATED_GB1183".to_string(), "/tmp".to_string());

        let spec = HookSpec {
            name: "test-undef".into(),
            event: crate::event::HookEventName::Stop,
            handler_type: crate::config::HandlerType::Command,
            configured_matcher: None,
            matcher: None,
            enabled: true,
            command: Some(std::path::PathBuf::from(
                "${NEVER_SET_GB1183}/does/not/exist.sh",
            )),
            command_raw: Some("${NEVER_SET_GB1183}/does/not/exist.sh".to_string()),
            url: None,
            url_raw: None,
            timeout_ms: 5000,
            source_dir: std::env::temp_dir(),
            extra_env,
            layer: crate::config::HookProvenance::File,
        };

        let envelope = make_envelope();
        let ctx = make_ctx();
        let (result, _) = run_command_hook(&spec, &envelope, &ctx, GateKind::Observe).await;

        match result {
            HookRunnerResult::Failed(reason) => {
                assert!(
                    reason.contains("NEVER_SET_GB1183"),
                    "failure reason should name the undefined env var, got: {reason}"
                );
                assert!(
                    reason.contains("hook not executed"),
                    "failure reason should make clear the hook did not run, got: {reason}"
                );
                assert!(
                    !reason.contains("exit code"),
                    "failure reason should not reference an exit code (we never spawned), got: {reason}"
                );
            }
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    /// Regression: a hook command starting with `~` must be
    /// routed through `sh -c` so the shell expands `~` to `$HOME`.
    /// Previously `~/.claude/hook.sh` was treated as a relative path and
    /// joined to `source_dir`, producing a broken path.
    ///
    /// The test injects `HOME` via `extra_env` so it works in sandboxed
    /// CI environments where `HOME` is not set (e.g. hermetic remote exec).
    #[tokio::test]
    #[cfg(unix)]
    async fn test_tilde_expansion_runs_via_shell() {
        let tmp = tempfile::tempdir().unwrap();
        // Create the script at <tmp>/.echo-agent-test-hooks-gb856/tilde-test.sh
        let hook_dir = tmp.path().join(".echo-agent-test-hooks-gb856");
        std::fs::create_dir_all(&hook_dir).unwrap();
        let script = hook_dir.join("tilde-test.sh");
        std::fs::write(&script, "#!/bin/sh\nexit 0\n").unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&script).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&script, perms).unwrap();
        }

        // Inject HOME via extra_env so `sh -c "~/.echo-agent-test-hooks-gb856/..."`
        // expands `~` to the temp dir. This avoids depending on the system
        // HOME, which is absent in hermetic sandboxed test runners.
        let mut extra_env = std::collections::HashMap::new();
        extra_env.insert(
            "HOME".to_string(),
            tmp.path().to_string_lossy().into_owned(),
        );

        let spec = HookSpec {
            name: "test-tilde".into(),
            event: crate::event::HookEventName::Stop,
            handler_type: crate::config::HandlerType::Command,
            configured_matcher: None,
            matcher: None,
            enabled: true,
            command: Some(std::path::PathBuf::from(
                "~/.echo-agent-test-hooks-gb856/tilde-test.sh",
            )),
            command_raw: Some("~/.echo-agent-test-hooks-gb856/tilde-test.sh".to_string()),
            url: None,
            url_raw: None,
            timeout_ms: 5000,
            source_dir: std::env::temp_dir(),
            extra_env,
            layer: crate::config::HookProvenance::File,
        };

        let envelope = make_envelope();
        let ctx = make_ctx();

        // Freshly writing the script and exec'ing it via `sh -c` can transiently
        // fail with ETXTBSY ("Text file busy" -> exit 126) when a sibling test in
        // this multi-threaded binary forks while our write fd is still open and
        // its child inherits it. Retry ONLY that exact transient; a real tilde-
        // routing break surfaces as a different result (127/spawn error), so the
        // assertion below keeps its diagnostic power.
        let mut result = run_command_hook(&spec, &envelope, &ctx, GateKind::Observe)
            .await
            .0;
        for _ in 0..8 {
            if !matches!(&result, HookRunnerResult::Failed(msg) if msg.starts_with("exit code 126"))
            {
                break;
            }
            result = run_command_hook(&spec, &envelope, &ctx, GateKind::Observe)
                .await
                .0;
        }

        assert!(
            matches!(result, HookRunnerResult::Success),
            "hook with ~/... path should be expanded via sh -c, got {:?}",
            result
        );
    }

    /// Hooks that explicitly handle the unset case via parameter expansion
    /// (e.g. `${VAR:-/some/default}`) must NOT be refused: the user has
    /// expressed intent for what should happen when the var is unset.
    #[tokio::test]
    async fn test_parameter_expansion_default_is_not_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let script = tmp.path().join("default.sh");
        std::fs::write(&script, "#!/bin/sh\nexit 0\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&script).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&script, perms).unwrap();
        }

        let spec = HookSpec {
            name: "test-default".into(),
            event: crate::event::HookEventName::Stop,
            handler_type: crate::config::HandlerType::Command,
            configured_matcher: None,
            matcher: None,
            enabled: true,
            // `MISSING_GB1183_DEFAULT` is intentionally unset; the `:-`
            // modifier supplies a fallback that points at the real script.
            command: Some(std::path::PathBuf::from(format!(
                "${{MISSING_GB1183_DEFAULT:-{}}}",
                script.display()
            ))),
            command_raw: Some(format!("${{MISSING_GB1183_DEFAULT:-{}}}", script.display())),
            url: None,
            url_raw: None,
            timeout_ms: 5000,
            source_dir: tmp.path().to_path_buf(),
            extra_env: std::collections::HashMap::new(),
            layer: crate::config::HookProvenance::File,
        };

        let envelope = make_envelope();
        let ctx = make_ctx();
        let (result, _) = run_command_hook(&spec, &envelope, &ctx, GateKind::Observe).await;

        assert!(
            matches!(result, HookRunnerResult::Success),
            "hook with parameter-expansion default must run, got {:?}",
            result
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn command_hook_session_close_reaps_whole_group() {
        let tmp = tempfile::tempdir().unwrap();
        let marker = tmp.path().join("grandchild_alive");
        // `& wait` keeps the leader alive while the grandchild outlives it, so
        // only a group kill stops the marker being written.
        let mut spec = make_shell_spec(&format!(
            "sh -c 'sleep 2 && echo alive > {}' & wait",
            marker.display()
        ));
        spec.timeout_ms = 60_000;
        let envelope = make_envelope();
        let scope = echo_agent_tools::util::ProcessScope::new();
        let hook_scope = scope.clone();
        let hook = tokio::spawn(async move {
            run_command_hook(
                &spec,
                &envelope,
                &make_scoped_ctx(hook_scope),
                GateKind::Observe,
            )
            .await
        });

        tokio::time::sleep(Duration::from_millis(800)).await;
        scope.kill_all();

        tokio::time::timeout(Duration::from_secs(15), hook)
            .await
            .expect("kill_all must reap the enrolled hook, not leave it on its 60s timeout")
            .expect("hook task join");

        tokio::time::sleep(Duration::from_secs(3)).await;
        assert!(
            !marker.exists(),
            "grandchild outlived session close, so the group was not killpg'd"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn command_hook_fails_fast_when_scope_already_closed() {
        let scope = echo_agent_tools::util::ProcessScope::new();
        scope.kill_all();
        let mut spec = make_shell_spec("sleep 600");
        spec.timeout_ms = 60_000;

        let (result, _) = tokio::time::timeout(
            Duration::from_secs(15),
            run_command_hook(
                &spec,
                &make_envelope(),
                &make_scoped_ctx(scope),
                GateKind::Observe,
            ),
        )
        .await
        .expect("a closed scope must fail the hook immediately, not run to its 60s timeout");

        assert!(
            matches!(result, HookRunnerResult::Failed(_)),
            "got {result:?}"
        );
    }
}
