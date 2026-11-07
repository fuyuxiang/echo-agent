//! An external, pipe-owned crash watcher. Normal exit/update preparation pauses
//! recovery; the helper explicitly breaks away from the kill-on-close host Job.
use serde::{Deserialize, Serialize};
use std::io::{BufRead, Read, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const HELPER: &str = "--echoagent-crash-watchdog";
const HISTORY: &str = "ECHOAGENT_CRASH_RECOVERY_HISTORY";
const RESTORED: &str = "--recovered-host";
const RESTORE_MARKER: &str = "ECHOAGENT_HOST_RECOVERED";
const WINDOW_SECONDS: u64 = 120;
const MAX_RESTARTS: usize = 3;

#[derive(Serialize, Deserialize)]
struct Launch {
    pid: u32,
    executable: std::path::PathBuf,
    cwd: std::path::PathBuf,
    args: Vec<String>,
    history: Vec<u64>,
    visible: bool,
}
struct Watcher {
    input: ChildStdin,
    _child: Child,
    _protection: Option<echo_agent_tools::util::process_safety::ProcessLease>,
}
static WATCHER: OnceLock<Mutex<Option<Watcher>>> = OnceLock::new();
static WAS_RESTORED: OnceLock<bool> = OnceLock::new();

/// Call at the executable entry, before logging/Tauri create any threads. The
/// environment marker is consumed so a later deliberate app.restart() does not
/// inherit a stale crash notice. No environment is mutated after startup.
pub fn consume_restore_marker() {
    let restored = std::env::var(RESTORE_MARKER).as_deref() == Ok("1");
    std::env::remove_var(RESTORE_MARKER);
    let _ = WAS_RESTORED.set(restored);
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn next_history(history: &[u64], time: u64) -> Option<Vec<u64>> {
    let mut history: Vec<_> = history
        .iter()
        .copied()
        .filter(|t| *t <= time && time - t < WINDOW_SECONDS)
        .collect();
    if history.len() >= MAX_RESTARTS {
        return None;
    }
    history.push(time);
    Some(history)
}

#[derive(Debug, PartialEq, Eq)]
enum OwnerEnd {
    Quit,
    Closed { intentional: bool, visible: bool },
}

fn read_owner_messages(reader: &mut impl BufRead, mut visible: bool) -> Result<OwnerEnd, String> {
    let mut intentional = false;
    loop {
        let mut line = String::new();
        let count = (&mut *reader)
            .take(1024)
            .read_line(&mut line)
            .map_err(|e| e.to_string())?;
        if count == 0 {
            return Ok(OwnerEnd::Closed {
                intentional,
                visible,
            });
        }
        if !line.ends_with('\n') {
            return Err("incomplete owner message".into());
        }
        match line.trim() {
            "quit" => return Ok(OwnerEnd::Quit),
            "intentional=1" => intentional = true,
            "intentional=0" => intentional = false,
            "visible=1" => visible = true,
            "visible=0" => visible = false,
            _ => return Err("invalid owner message".into()),
        }
    }
}

/// Called before logging, runtime, WebView, single-instance or host-Job startup.
pub fn run_helper_if_requested() -> bool {
    if std::env::args_os().nth(1).as_deref() != Some(std::ffi::OsStr::new(HELPER)) {
        return false;
    }
    if let Err(error) = helper() {
        eprintln!("EchoAgent crash watcher: {error}");
    }
    true
}

fn helper() -> Result<(), String> {
    let mut reader = std::io::stdin().lock();
    let mut bytes = Vec::new();
    // Bound the pipe protocol before parsing. No executable or shell supplied by
    // a renderer/model is accepted: only relaunch this exact installed binary.
    let mut first = (&mut reader).take(64 * 1024);
    first
        .read_until(b'\n', &mut bytes)
        .map_err(|e| e.to_string())?;
    if !bytes.ends_with(b"\n") {
        return Err("incomplete owner launch record".into());
    }
    let line = String::from_utf8(bytes).map_err(|e| e.to_string())?;
    let mut launch: Launch = serde_json::from_str(&line).map_err(|e| e.to_string())?;
    let executable = std::env::current_exe()
        .and_then(std::fs::canonicalize)
        .map_err(|e| e.to_string())?;
    if launch.executable != executable
        || !launch.cwd.is_absolute()
        || launch.pid == std::process::id()
        || launch.args.iter().any(|arg| arg == HELPER)
    {
        return Err("invalid owner launch identity".into());
    }
    match read_owner_messages(&mut reader, launch.visible)? {
        OwnerEnd::Quit
        | OwnerEnd::Closed {
            intentional: true, ..
        } => return Ok(()),
        OwnerEnd::Closed { visible, .. } => launch.visible = visible,
    }
    // A closed pipe must not create a second instance while the owner is alive.
    for _ in 0..40 {
        if !owner_alive(launch.pid) {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    if owner_alive(launch.pid) {
        return Ok(());
    }
    let Some(history) = next_history(&launch.history, now()) else {
        #[cfg(target_os = "macos")]
        {
            let _ = Command::new("/usr/bin/osascript")
                .args(["-e", "display dialog \"EchoAgent 短时间内多次异常退出，已停止自动重启。请手动重新打开应用；若仍失败，请保留应用日志以便诊断。\" with title \"EchoAgent 恢复提示\" buttons {\"知道了\"} default button 1 with icon caution"])
                .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status();
        }
        #[cfg(windows)]
        {
            use windows_sys::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONWARNING, MB_OK};
            let message: Vec<u16> = "EchoAgent 短时间内多次异常退出，已停止自动重启。请手动重新打开应用；若仍失败，请保留应用日志以便诊断。\0".encode_utf16().collect();
            let title: Vec<u16> = "EchoAgent 恢复提示\0".encode_utf16().collect();
            unsafe {
                MessageBoxW(
                    std::ptr::null_mut(),
                    message.as_ptr(),
                    title.as_ptr(),
                    MB_OK | MB_ICONWARNING,
                );
            }
        }
        return Ok(());
    };
    std::thread::sleep(Duration::from_millis([500, 1500, 4000][history.len() - 1]));
    launch
        .args
        .retain(|arg| arg != RESTORED && arg != "--background");
    if !launch.visible {
        launch.args.push("--background".into());
    }
    let cwd = if launch.cwd.is_dir() {
        launch.cwd
    } else {
        // The original launch directory may have been removed while the app
        // was running; that must not prevent crash recovery of the binary.
        executable
            .parent()
            .ok_or("crash watcher executable has no directory")?
            .to_path_buf()
    };
    let mut command = Command::new(executable);
    command
        .args(launch.args)
        .current_dir(cwd)
        .env(RESTORE_MARKER, "1")
        .env(
            HISTORY,
            serde_json::to_string(&history).map_err(|e| e.to_string())?,
        )
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000 | 0x0000_0008);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    command.spawn().map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(unix)]
fn owner_alive(pid: u32) -> bool {
    unsafe {
        libc::kill(pid as i32, 0) == 0
            || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
}
#[cfg(windows)]
fn owner_alive(pid: u32) -> bool {
    use windows_sys::Win32::{
        Foundation::CloseHandle,
        System::Threading::{OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE},
    };
    let handle = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
    if handle.is_null() {
        return false;
    }
    let alive = unsafe { WaitForSingleObject(handle, 0) } == 258;
    unsafe { CloseHandle(handle) };
    alive
}
#[cfg(not(any(unix, windows)))]
fn owner_alive(_: u32) -> bool {
    true
}

pub fn start() -> Result<(), String> {
    // Cargo/dev-server stop and acceptance tools must remain deterministic.
    if cfg!(debug_assertions)
        || std::env::var_os("ECHO_DESKTOP_REVIEW").is_some()
        || std::env::var_os("ECHO_DESKTOP_VALIDATION").is_some()
    {
        return Ok(());
    }
    start_owned()
}

// Called only by the fixed, private native lifecycle acceptance fixture after
// its origin, model configuration and owned paths have all been validated.
pub(crate) fn start_for_native_review() -> Result<u32, String> {
    if !cfg!(debug_assertions) {
        return Err("Native lifecycle review requires a debug build".into());
    }
    start_owned()?;
    WATCHER
        .get()
        .and_then(|w| w.lock().ok()?.as_ref().map(|w| w._child.id()))
        .ok_or_else(|| "Crash watcher was not started".into())
}

fn start_owned() -> Result<(), String> {
    let executable = std::env::current_exe()
        .and_then(std::fs::canonicalize)
        .map_err(|e| e.to_string())?;
    let args = std::env::args_os()
        .skip(1)
        .map(|arg| {
            arg.into_string()
                .map_err(|_| "启动参数不是有效 Unicode".to_string())
        })
        .collect::<Result<Vec<_>, _>>()?;
    let history: Vec<u64> = std::env::var(HISTORY)
        .ok()
        .and_then(|v| serde_json::from_str(&v).ok())
        .unwrap_or_default();
    let launch = Launch {
        pid: std::process::id(),
        executable: executable.clone(),
        cwd: std::env::current_dir().map_err(|e| e.to_string())?,
        visible: !args.iter().any(|arg| arg == "--background"),
        args,
        history,
    };
    let mut command = Command::new(executable);
    command
        .arg(HELPER)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000 | 0x0100_0000);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child =
        crate::process_supervisor::spawn_crash_watcher(&mut command).map_err(|e| e.to_string())?;
    let result = (|| {
        let mut input = child
            .stdin
            .take()
            .ok_or("crash watcher input unavailable")?;
        serde_json::to_writer(&mut input, &launch).map_err(|e| e.to_string())?;
        input.write_all(b"\n").map_err(|e| e.to_string())?;
        let protection = echo_agent_tools::util::process_safety::protect_tree(child.id())?;
        Ok::<_, String>((input, protection))
    })();
    match result {
        Ok((input, protection)) => {
            *WATCHER
                .get_or_init(|| Mutex::new(None))
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = Some(Watcher {
                input,
                _child: child,
                _protection: protection,
            });
            Ok(())
        }
        Err(error) => {
            let _ = child.kill();
            let _ = child.wait();
            Err(error)
        }
    }
}

fn send(message: &[u8]) {
    if let Some(watcher) = WATCHER.get() {
        if let Some(watcher) = watcher.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
            if let Err(error) = watcher.input.write_all(message) {
                tracing::warn!(%error, "crash watcher pipe unavailable");
            }
        }
    }
}
pub fn prepare_exit() {
    send(b"intentional=1\n");
}
pub fn cancel_exit() {
    send(b"intentional=0\n");
}
pub fn visibility(visible: bool) {
    send(if visible {
        b"visible=1\n"
    } else {
        b"visible=0\n"
    });
}
pub fn finish_exit() {
    send(b"quit\n");
}
pub fn restored() -> bool {
    WAS_RESTORED.get().copied().unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn host_crash_loop_has_a_rolling_budget_and_clock_changes_do_not_lock_it() {
        assert_eq!(next_history(&[], 100), Some(vec![100]));
        assert_eq!(next_history(&[100, 101, 102], 103), None);
        assert_eq!(next_history(&[100, 101, 102], 221), Some(vec![102, 221]));
        assert_eq!(next_history(&[500], 100), Some(vec![100]));
    }
    #[test]
    fn intentional_exit_update_and_cancelled_exit_have_distinct_owner_outcomes() {
        fn read(input: &[u8]) -> OwnerEnd {
            read_owner_messages(&mut std::io::Cursor::new(input), true).unwrap()
        }
        assert_eq!(read(b"quit\n"), OwnerEnd::Quit);
        assert_eq!(
            read(b"intentional=1\n"),
            OwnerEnd::Closed {
                intentional: true,
                visible: true
            }
        );
        assert_eq!(
            read(b"intentional=1\nintentional=0\nvisible=0\n"),
            OwnerEnd::Closed {
                intentional: false,
                visible: false
            }
        );
        assert_eq!(
            read(b""),
            OwnerEnd::Closed {
                intentional: false,
                visible: true
            }
        );
        assert!(read_owner_messages(&mut std::io::Cursor::new(b"intentional=0"), true).is_err());
        assert!(read_owner_messages(&mut std::io::Cursor::new(vec![b'x'; 2048]), true).is_err());
    }
}
