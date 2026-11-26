pub mod changelog;
pub mod echo_agent_home;
pub mod event_id;
pub mod secure_file;
pub mod tips;
pub mod uname;
pub use echo_agent_shared::clipboard;
pub use echo_agent_shared::stderr::{stderr_lock, with_locked_stderr};
/// Generate a pseudo-random f64 in [0.0, 1.0).
///
/// Uses `RandomState::new()` which is OS-seeded (via `getrandom`) on each
/// instantiation, producing a unique hasher state per call. A fixed sentinel
/// is hashed to extract the random bits — the entropy comes entirely from
/// the OS-seeded `RandomState`, not from any clock source.
///
/// # Precision
/// The result uses all 53 bits of `f64` mantissa for a uniform distribution
/// over `[0.0, 1.0)`. We shift the 64-bit hash right by 11 bits to get a
/// 53-bit integer, then divide by `2^53`. This avoids the subtle bias that
/// occurs when casting a full `u64` to `f64` (which has only 52 bits of
/// mantissa, causing multiple `u64` values to map to the same `f64` for
/// values > 2^52).
///
/// Not cryptographically secure — suitable for sampling and feature
/// rollouts, not for security-sensitive randomness.
pub fn random_f64() -> f64 {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    let random_state = RandomState::new();
    let mut hasher = random_state.build_hasher();
    hasher.write_u64(0x517cc1b727220a95);
    (hasher.finish() >> 11) as f64 / (1u64 << 53) as f64
}
/// Probabilistic sampling. Returns `true` with probability `rate` (0.0–1.0).
pub fn probabilistic_sample(rate: f64) -> bool {
    random_f64() < rate
}
fn matches_trusted_base_url(candidate: &str, trusted_base: &str) -> bool {
    let Ok(candidate) = reqwest::Url::parse(candidate) else {
        return false;
    };
    let Ok(trusted) = reqwest::Url::parse(trusted_base) else {
        return false;
    };
    let trusted_path = trusted.path();
    let candidate_path = candidate.path();
    let path_matches = candidate_path == trusted_path
        || candidate_path
            .strip_prefix(trusted_path)
            .is_some_and(|suffix| suffix.starts_with('/'));
    candidate.scheme() == trusted.scheme()
        && candidate.host_str() == trusted.host_str()
        && candidate.port_or_known_default() == trusted.port_or_known_default()
        && path_matches
}
/// Production cli-chat-proxy base only (compiled-in constant).
///
/// Unlike [`is_cli_chat_proxy_url`], this rejects loopback and staging/dev hosts.
/// Used for security-sensitive remote kill-switches that must not become env
/// toggles via `ECHO_AGENT_CLI_CHAT_PROXY_BASE_URL` (or similar) pointing at an
/// attacker-controlled origin.
pub fn is_prod_cli_chat_proxy_url(url: &str) -> bool {
    matches_trusted_base_url(url, crate::env::PROD_CLI_CHAT_PROXY_BASE_URL)
}
/// True for configured first-party cli-chat-proxy routes, excluding arbitrary loopback URLs.
///
/// Unlike [`is_cli_chat_proxy_url`], this only trusts the exact compiled or
/// environment-selected route and is suitable for EchoAgent-only request extensions.
pub fn is_trusted_cli_chat_proxy_url(url: &str) -> bool {
    if is_prod_cli_chat_proxy_url(url) {
        return true;
    }
    false
}
/// True for cli-chat-proxy URLs (production, plus local-dev hosts when the
/// optional non-production feature is enabled). When that feature is on,
/// runtime env overrides can extend this trust set. Loopback is always
/// accepted (unit tests and local mock servers on arbitrary ports).
pub fn is_cli_chat_proxy_url(url: &str) -> bool {
    if is_trusted_cli_chat_proxy_url(url) {
        return true;
    }
    if let Ok(u) = reqwest::Url::parse(url)
        && let Some(h) = u.host_str()
        && (h == "localhost" || h == "127.0.0.1" || h == "::1")
    {
        return true;
    }
    false
}
/// True for EchoAgent-operated endpoints (`*.echo-agent.invalid`, cli-chat-proxy, and optional
/// non-production EchoAgent hosts when that feature is enabled).
/// `disable_api_key_auth` refuses keys only for these; other hosts are BYOK and
/// exempt. Safe against invalid URLs and suffix attacks (`evil-echo.agent.example`).
///
/// Scheme-agnostic so credential *refusal* fails closed. To decide where to
/// *attach* a credential, use [`is_echo_agent_api_bearer_url`].
pub fn is_echo_agent_api_url(url: &str) -> bool {
    is_echo_agent_api_url_impl(url, false)
}
/// Like [`is_echo_agent_api_url`], but requires `https` on every arm, so a
/// session bearer is never attached to a cleartext endpoint, including loopback
/// (a co-located process could otherwise read a token sent to `http://localhost`).
pub fn is_echo_agent_api_bearer_url(url: &str) -> bool {
    if is_trusted_echo_agent_https_url(url) {
        return true;
    }
    false
}
/// True for trusted first-party EchoAgent HTTPS routes, excluding arbitrary loopback URLs.
pub fn is_trusted_echo_agent_https_url(url: &str) -> bool {
    let Ok(parsed) = reqwest::Url::parse(url) else {
        return false;
    };
    if parsed.scheme() != "https" {
        return false;
    }
    if is_loopback_host(&parsed) {
        return false;
    }
    if is_trusted_cli_chat_proxy_url(url) {
        return true;
    }
    parsed
        .host_str()
        .is_some_and(|host| host == "echo.agent" || host.ends_with(".echo-agent.invalid"))
}
fn is_echo_agent_api_url_impl(url: &str, require_https: bool) -> bool {
    if require_https {
        return is_echo_agent_api_bearer_url(url);
    }
    if is_cli_chat_proxy_url(url) {
        return true;
    }
    reqwest::Url::parse(url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_owned))
        .is_some_and(|host| host == "echo.agent" || host.ends_with(".echo-agent.invalid"))
}
fn is_loopback_host(parsed: &reqwest::Url) -> bool {
    match parsed.host() {
        Some(url::Host::Domain(host)) => host == "localhost",
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    }
}
/// Truncate a string to at most `max_chars` characters.
/// Slices at char boundaries so multi-byte UTF-8 never panics.
pub fn truncate(s: &str, max_chars: usize) -> &str {
    if s.len() <= max_chars {
        return s;
    }
    let end = s
        .char_indices()
        .nth(max_chars)
        .map(|(i, _)| i)
        .unwrap_or(s.len());
    &s[..end]
}
/// Check if a process is still alive.
///
/// - Unix: `kill(pid, 0)` via `nix`. True if the process exists (even
///   under a different UID); false only on ESRCH.
/// - Windows: `OpenProcess(SYNCHRONIZE)` + `WaitForSingleObject(0)`. True
///   while running; false on exit, absence, or open failure.
#[cfg(unix)]
pub fn is_process_alive(pid: u32) -> bool {
    use nix::errno::Errno;
    use nix::sys::signal::kill;
    use nix::unistd::Pid;
    match kill(Pid::from_raw(pid as i32), None) {
        Ok(()) => true,
        Err(Errno::ESRCH) => false,
        Err(_) => true,
    }
}
#[cfg(windows)]
pub fn is_process_alive(pid: u32) -> bool {
    use windows::Win32::Foundation::{CloseHandle, WAIT_TIMEOUT};
    use windows::Win32::System::Threading::{
        OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject,
    };
    let Ok(handle) = (unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, pid) }) else {
        return false;
    };
    let wait_result = unsafe { WaitForSingleObject(handle, 0) };
    let _ = unsafe { CloseHandle(handle) };
    wait_result == WAIT_TIMEOUT
}
/// Which termination signal to send. On Windows both map to `TerminateProcess`
/// (already forceful), so the distinction only matters on Unix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KillSignal {
    /// Graceful `SIGTERM` (Unix) — the process may catch and drain.
    Term,
    /// Forceful `SIGKILL` (Unix) — unblockable escalation.
    Kill,
}
/// Terminate a process by PID with `SIGTERM`. Idempotent: already-dead is `Ok`.
pub fn kill_process_by_pid(pid: u32) -> std::io::Result<()> {
    kill_process_with_signal(pid, KillSignal::Term)
}
/// Terminate a process by PID with a chosen signal. Idempotent: already-dead is `Ok`.
///
/// - Unix: `SIGTERM`/`SIGKILL` via `nix::sys::signal::kill`; ESRCH maps to `Ok`.
/// - Windows: `OpenProcess(PROCESS_TERMINATE)` + `TerminateProcess` (already
///   forceful, so `signal` is ignored); ERROR_INVALID_PARAMETER maps to `Ok`.
pub fn kill_process_with_signal(pid: u32, signal: KillSignal) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use nix::errno::Errno;
        use nix::sys::signal::{Signal, kill};
        use nix::unistd::Pid;
        let sig = match signal {
            KillSignal::Term => Signal::SIGTERM,
            KillSignal::Kill => Signal::SIGKILL,
        };
        match kill(Pid::from_raw(pid as i32), sig) {
            Ok(()) | Err(Errno::ESRCH) => Ok(()),
            Err(e) => Err(std::io::Error::from_raw_os_error(e as i32)),
        }
    }
    #[cfg(windows)]
    {
        let _ = signal;
        use windows::Win32::Foundation::{CloseHandle, ERROR_INVALID_PARAMETER};
        use windows::Win32::System::Threading::{OpenProcess, PROCESS_TERMINATE, TerminateProcess};
        use windows::core::HRESULT;
        let no_such_process = HRESULT::from_win32(ERROR_INVALID_PARAMETER.0);
        let handle = match unsafe { OpenProcess(PROCESS_TERMINATE, false, pid) } {
            Ok(h) => h,
            Err(e) if e.code() == no_such_process => return Ok(()),
            Err(e) => {
                return Err(std::io::Error::other(format!("OpenProcess({pid}): {e}")));
            }
        };
        let terminate = unsafe { TerminateProcess(handle, 0) };
        let _ = unsafe { CloseHandle(handle) };
        terminate.map_err(|e| std::io::Error::other(format!("TerminateProcess({pid}): {e}")))
    }
}
/// True if `pid` is a echoagent process; pairs with [`kill_process_by_pid`] to avoid killing a recycled PID.
/// Best-effort on macOS/BSD (liveness-only via `kill -0`), exact on Linux (/proc cmdline) and Windows (image path).
pub fn is_echo_agent_process(pid: u32) -> bool {
    #[cfg(target_os = "linux")]
    {
        let cmdline_path = format!("/proc/{pid}/cmdline");
        match std::fs::read(&cmdline_path) {
            Ok(data) => String::from_utf8_lossy(&data).contains("echoagent"),
            Err(_) => false,
        }
    }
    #[cfg(windows)]
    {
        use windows::Win32::Foundation::CloseHandle;
        use windows::Win32::System::Threading::{
            OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
            QueryFullProcessImageNameW,
        };
        use windows::core::PWSTR;
        let Ok(handle) = (unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) })
        else {
            return false;
        };
        let mut buf: Vec<u16> = vec![0; 1024];
        let mut size: u32 = buf.len() as u32;
        let result = unsafe {
            QueryFullProcessImageNameW(
                handle,
                PROCESS_NAME_WIN32,
                PWSTR(buf.as_mut_ptr()),
                &mut size,
            )
        };
        let _ = unsafe { CloseHandle(handle) };
        if result.is_err() {
            return false;
        }
        String::from_utf16_lossy(&buf[..size as usize])
            .to_ascii_lowercase()
            .contains("echoagent")
    }
    #[cfg(all(not(target_os = "linux"), not(windows)))]
    {
        let mut cmd = std::process::Command::new("kill");
        cmd.args(["-0", &pid.to_string()])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        echo_agent_tty_utils::detach_std_command(&mut cmd);
        cmd.status().is_ok_and(|s| s.success())
    }
}
/// Stricter [`is_echo_agent_process`] for the auto-kill zombie path: on macOS/BSD it
/// name-matches via `ps` (not liveness-only), so eviction never SIGKILLs a
/// recycled PID now owned by an unrelated process. Linux/Windows already match
/// exactly, so this delegates there. Use the permissive [`is_echo_agent_process`] for
/// operator-driven `echoagent leaders kill`.
pub fn is_echo_agent_process_strict(pid: u32) -> bool {
    #[cfg(all(not(target_os = "linux"), not(windows)))]
    {
        let mut cmd = std::process::Command::new("ps");
        cmd.args(["-p", &pid.to_string(), "-o", "comm="])
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        echo_agent_tty_utils::detach_std_command(&mut cmd);
        match cmd.output() {
            Ok(out) if out.status.success() => {
                let comm = String::from_utf8_lossy(&out.stdout);
                comm.lines()
                    .next()
                    .map(str::trim)
                    .filter(|line| !line.is_empty())
                    .and_then(|line| std::path::Path::new(line).file_name())
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.to_ascii_lowercase().contains("echoagent"))
            }
            _ => false,
        }
    }
    #[cfg(any(target_os = "linux", windows))]
    {
        is_echo_agent_process(pid)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn test_is_cli_chat_proxy_url_accepts_proxy_subpath() {
        assert!(is_cli_chat_proxy_url(
            "https://chat-proxy.echo-agent.invalid/v1/chat/completions"
        ));
    }
    #[test]
    fn test_is_cli_chat_proxy_url_rejects_public_api() {
        assert!(!is_cli_chat_proxy_url("https://api.echo-agent.invalid/v1"));
    }
    #[test]
    fn test_is_cli_chat_proxy_url_rejects_spoofed_hostname() {
        assert!(!is_cli_chat_proxy_url(
            "https://chat-proxy.echo-agent.invalid.evil.example/v1"
        ));
    }
    #[test]
    fn test_is_cli_chat_proxy_url_rejects_v11_prefix_confusion() {
        assert!(!is_cli_chat_proxy_url(
            "https://chat-proxy.echo-agent.invalid/v11/chat/completions"
        ));
    }
    #[test]
    fn test_is_echo_agent_api_url() {
        assert!(is_echo_agent_api_url("https://api.echo-agent.invalid/v1"));
        assert!(is_echo_agent_api_url(
            "https://api.echo-agent.invalid/v1/chat/completions"
        ));
        assert!(is_echo_agent_api_url("https://echo-agent.invalid"));
        assert!(is_echo_agent_api_url(
            "https://chat-proxy.echo-agent.invalid/v1/chat/completions"
        ));
        assert!(!is_echo_agent_api_url("https://api.openai.com/v1"));
        assert!(!is_echo_agent_api_url("https://api.anthropic.com/v1"));
        assert!(!is_echo_agent_api_url(
            "https://generativelanguage.googleapis.com"
        ));
        assert!(!is_echo_agent_api_url(
            "https://api.echo-agent.invalid.evil.example/v1"
        ));
        assert!(!is_echo_agent_api_url(
            "https://evil-echo.agent.attacker.com/v1"
        ));
        assert!(!is_echo_agent_api_url("https://prefixecho.agent/v1"));
        assert!(!is_echo_agent_api_url("not-a-url"));
        assert!(!is_echo_agent_api_url(""));
        assert!(is_echo_agent_api_url("http://api.echo-agent.invalid/v1"));
        assert!(is_echo_agent_api_url("http://localhost:11434/v1"));
    }
    #[test]
    fn test_is_echo_agent_api_bearer_url() {
        assert!(is_echo_agent_api_bearer_url(
            "https://api.echo-agent.invalid/v1"
        ));
        assert!(!is_echo_agent_api_bearer_url(
            "http://api.echo-agent.invalid/v1"
        ));
        assert!(!is_echo_agent_api_bearer_url("http://localhost:11434/v1"));
        {
            assert!(!is_echo_agent_api_bearer_url("https://localhost:11434/v1"));
            assert!(!is_echo_agent_api_bearer_url("https://127.0.0.2:11434/v1"));
            assert!(!is_echo_agent_api_bearer_url("https://[::1]:11434/v1"));
        }
        assert!(is_echo_agent_api_bearer_url(
            "https://API.echo-agent.invalid/v1"
        ));
        assert!(!is_echo_agent_api_bearer_url(
            "https://api.echo-agent.invalid@attacker.example/v1"
        ));
        assert!(!is_echo_agent_api_bearer_url("https://х.ai/v1"));
    }
    #[test]
    fn test_truncate() {
        assert_eq!(truncate("hello", 5), "hello");
        assert_eq!(truncate("hello world", 5), "hello");
        assert_eq!(truncate("abc🎉🎉def", 5), "abc🎉🎉");
    }
    #[test]
    fn is_process_alive_current_process() {
        assert!(is_process_alive(std::process::id()));
    }
    #[test]
    fn is_process_alive_dead_pid() {
        assert!(!is_process_alive(4_000_000_000));
    }
    #[cfg(unix)]
    #[test]
    fn is_process_alive_init_process() {
        assert!(is_process_alive(1));
    }
    #[test]
    fn kill_process_by_pid_already_dead_is_ok() {
        assert!(kill_process_by_pid(4_000_000_000).is_ok());
    }
    #[cfg(unix)]
    #[test]
    fn kill_process_by_pid_terminates_live_child() {
        #[allow(clippy::disallowed_methods)]
        let mut child = std::process::Command::new("sleep")
            .arg("60")
            .spawn()
            .expect("spawn sleep");
        let pid = child.id();
        kill_process_by_pid(pid).expect("kill should succeed");
        let status = child.wait().expect("wait child");
        assert!(
            !status.success(),
            "sleep was terminated, not exited cleanly"
        );
    }
    #[test]
    fn is_echo_agent_process_self_true_impossible_pid_false() {
        assert!(is_echo_agent_process(std::process::id()));
        assert!(!is_echo_agent_process(u32::MAX));
    }
    #[test]
    fn is_echo_agent_process_strict_self_true_impossible_pid_false() {
        assert!(is_echo_agent_process_strict(std::process::id()));
        assert!(!is_echo_agent_process_strict(u32::MAX));
    }
    #[cfg(unix)]
    #[test]
    fn kill_process_with_signal_sigkill_terminates_live_child() {
        #[allow(clippy::disallowed_methods)]
        let mut child = std::process::Command::new("sleep")
            .arg("60")
            .spawn()
            .expect("spawn sleep");
        let pid = child.id();
        kill_process_with_signal(pid, KillSignal::Kill).expect("sigkill should succeed");
        let status = child.wait().expect("wait child");
        assert!(!status.success(), "sleep was killed, not exited cleanly");
    }
}
