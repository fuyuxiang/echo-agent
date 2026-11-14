//! Same signed executable, private inherited pipes, bounded protocol and kill/reap deadline.
use super::*;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
const MAX_LINE: u64 = 2 * 1024 * 1024;
const DEADLINE: Duration = Duration::from_secs(20);

pub struct Process {
    child: Child,
    input: ChildStdin,
    replies: mpsc::Receiver<String>,
    secret: String,
}
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
fn read_frame(reader: &mut impl BufRead) -> Result<Option<String>, String> {
    let mut line = String::new();
    let size = std::io::Read::take(&mut *reader, MAX_LINE + 1)
        .read_line(&mut line)
        .map_err(|e| format!("helper_transport: {e}"))?;
    if size == 0 {
        return Ok(None);
    }
    if size as u64 > MAX_LINE || !line.ends_with('\n') {
        return Err("helper_protocol: oversized or incomplete frame".into());
    }
    Ok(Some(line))
}
impl Process {
    pub fn new() -> Result<Self, String> {
        let executable = std::env::current_exe().map_err(|e| e.to_string())?;
        let mut command = Command::new(executable);
        command
            .arg("--echo-native-helper")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        #[cfg(target_os = "windows")]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x08000000);
        }
        let mut child = command
            .spawn()
            .map_err(|e| format!("helper_start_failed: {e}"))?;
        let input = child.stdin.take().ok_or("helper_pipe_unavailable")?;
        let output = child.stdout.take().ok_or("helper_pipe_unavailable")?;
        let (sender, replies) = mpsc::sync_channel(2);
        std::thread::spawn(move || {
            let mut reader = BufReader::new(output);
            while let Ok(Some(line)) = read_frame(&mut reader) {
                if sender.send(line).is_err() {
                    break;
                }
            }
        });
        let mut process = Self {
            child,
            input,
            replies,
            secret: identity(),
        };
        process.send(&json!({"hello":process.secret,"version":1}))?;
        let hello = process
            .replies
            .recv_timeout(Duration::from_secs(3))
            .map_err(|_| "helper_handshake_timeout")?;
        let hello: Value = serde_json::from_str(&hello).map_err(|_| "helper_invalid_handshake")?;
        if hello["hello"] != process.secret || hello["version"] != 1 {
            return Err("helper_invalid_identity".into());
        }
        Ok(process)
    }
    fn send(&mut self, value: &Value) -> Result<(), String> {
        let bytes = serde_json::to_vec(value).map_err(|e| e.to_string())?;
        if bytes.len() as u64 >= MAX_LINE {
            return Err("helper_protocol: request too large".into());
        }
        self.input
            .write_all(&bytes)
            .and_then(|_| self.input.write_all(b"\n"))
            .and_then(|_| self.input.flush())
            .map_err(|_| "helper_disconnected: observe before retry".into())
    }
    pub fn call(
        &mut self,
        method: &str,
        args: &Value,
        token: &CancellationToken,
    ) -> Result<Value, String> {
        let id = identity();
        self.send(&json!({"secret":self.secret,"id":id,"method":method,"args":args}))?;
        let started = Instant::now();
        let mut cancelled = None;
        loop {
            if token.is_cancelled() && cancelled.is_none() {
                self.send(&json!({"secret":self.secret,"cancel":true}))?;
                cancelled = Some(Instant::now());
            }
            if started.elapsed() > DEADLINE
                || cancelled.is_some_and(|t: Instant| t.elapsed() > Duration::from_millis(750))
            {
                let _ = self.child.kill();
                let _ = self.child.wait();
                return Err("native_timeout: isolated helper terminated and reaped; action result unknown; refresh target before retry".into());
            }
            match self.replies.recv_timeout(Duration::from_millis(25)) {
                Ok(line) => {
                    let value: Value =
                        serde_json::from_str(&line).map_err(|_| "helper_invalid_response")?;
                    if value["id"] != id || value["secret"] != self.secret {
                        return Err("helper_invalid_response_identity".into());
                    }
                    if cancelled.is_some() {
                        return Err("cancelled: native helper acknowledged cancellation; observe before retry".into());
                    }
                    return if let Some(error) = value["error"].as_str() {
                        Err(error.into())
                    } else {
                        Ok(value["result"].clone())
                    };
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(_) => {
                    return Err(
                        "native_failure: isolated helper exited; result unknown; refresh discovery"
                            .into(),
                    )
                }
            }
        }
    }
}

pub fn main() -> Result<(), String> {
    if !trusted_parent() {
        return Err("helper_parent_denied: launch only from the desktop host".into());
    }
    let mut reader = BufReader::new(std::io::stdin());
    let hello = read_frame(&mut reader)?.ok_or("helper_missing_handshake")?;
    let hello: Value = serde_json::from_str(&hello).map_err(|_| "helper_invalid_handshake")?;
    let secret = hello["hello"]
        .as_str()
        .filter(|s| s.len() == 36)
        .ok_or("helper_invalid_handshake")?
        .to_owned();
    if hello["version"] != 1 {
        return Err("helper_invalid_version".into());
    }
    let emit = |value: &Value| -> Result<(), String> {
        let mut output = std::io::stdout().lock();
        serde_json::to_writer(&mut output, value).map_err(|e| e.to_string())?;
        output
            .write_all(b"\n")
            .and_then(|_| output.flush())
            .map_err(|e| e.to_string())
    };
    emit(&hello)?;
    let active = std::sync::Arc::new(std::sync::Mutex::new(CancellationToken::new()));
    let (sender, requests) = mpsc::sync_channel(2);
    let cancellation = active.clone();
    let expected = secret.clone();
    std::thread::spawn(move || {
        while let Ok(Some(line)) = read_frame(&mut reader) {
            let Ok(value) = serde_json::from_str::<Value>(&line) else {
                break;
            };
            if value["secret"] != expected {
                break;
            }
            if value["cancel"] == true {
                cancellation.lock().unwrap().cancel();
                continue;
            }
            let token = CancellationToken::new();
            *cancellation.lock().unwrap() = token.clone();
            if sender.send((value, token)).is_err() {
                break;
            }
        }
        cancellation.lock().unwrap().cancel();
        // Parent crash/exit closes the private pipe. Even a provider ignoring
        // cancellation must not leave an orphan capable of late UI actions.
        drop(sender);
        std::thread::sleep(Duration::from_millis(750));
        std::process::exit(0);
    });
    // Transport can be checked without acquiring OS accessibility privileges.
    // AX/UIA objects are created lazily, exclusively on this thread.
    let mut context = None;
    while let Ok((request, token)) = requests.recv_timeout(REF_AGE) {
        let method = request["method"].as_str().ok_or("helper_invalid_method")?;
        if method == "_helper_ping" {
            emit(
                &json!({"secret":secret,"id":request["id"],"result":{"protocol":1,"processId":std::process::id()}}),
            )?;
            continue;
        }
        if context.is_none() {
            let backend = match Platform::new() {
                Ok(backend) => backend,
                Err(error) => {
                    emit(&json!({"secret":secret,"id":request["id"],"error":error}))?;
                    continue;
                }
            };
            context = Some(Context {
                backend,
                refs: HashMap::new(),
                approvals: HashMap::new(),
                pages: HashMap::new(),
            });
        }
        let context = context.as_mut().unwrap();
        #[cfg(target_os = "macos")]
        let result = objc2::rc::autoreleasepool(|_| context.run(method, &request["args"], &token));
        #[cfg(not(target_os = "macos"))]
        let result = context.run(method, &request["args"], &token);
        emit(&match result {
            Ok(value) => json!({"secret":secret,"id":request["id"],"result":value}),
            Err(error) => json!({"secret":secret,"id":request["id"],"error":error}),
        })?;
    }
    Ok(())
}

fn trusted_parent() -> bool {
    let Ok(executable) = std::env::current_exe().and_then(|p| p.canonicalize()) else {
        return false;
    };
    #[cfg(target_os = "macos")]
    {
        unsafe extern "C" {
            fn proc_pidpath(pid: i32, buffer: *mut std::ffi::c_void, size: u32) -> i32;
        }
        let mut bytes = vec![0u8; 4096];
        let n = unsafe {
            proc_pidpath(
                libc::getppid(),
                bytes.as_mut_ptr().cast(),
                bytes.len() as u32,
            )
        };
        if n <= 0 {
            return false;
        }
        let end = bytes.iter().position(|b| *b == 0).unwrap_or(n as usize);
        use std::os::unix::ffi::OsStrExt;
        std::path::Path::new(std::ffi::OsStr::from_bytes(&bytes[..end]))
            .canonicalize()
            .ok()
            .as_ref()
            == Some(&executable)
    }
    #[cfg(target_os = "linux")]
    {
        std::fs::read_link(format!("/proc/{}/exe", unsafe { libc::getppid() }))
            .ok()
            .and_then(|p| p.canonicalize().ok())
            .as_ref()
            == Some(&executable)
    }
    #[cfg(target_os = "windows")]
    unsafe {
        use windows_sys::Win32::{
            Foundation::{CloseHandle, INVALID_HANDLE_VALUE},
            System::{Diagnostics::ToolHelp::*, Threading::*},
        };
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snapshot == INVALID_HANDLE_VALUE {
            return false;
        }
        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = std::mem::size_of_val(&entry) as u32;
        let pid = GetCurrentProcessId();
        let mut parent = 0;
        let mut found = Process32FirstW(snapshot, &mut entry);
        while found != 0 {
            if entry.th32ProcessID == pid {
                parent = entry.th32ParentProcessID;
                break;
            }
            found = Process32NextW(snapshot, &mut entry);
        }
        CloseHandle(snapshot);
        if parent == 0 {
            return false;
        }
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, parent);
        if handle.is_null() {
            return false;
        }
        let mut text = vec![0u16; 32768];
        let mut size = text.len() as u32;
        let ok = QueryFullProcessImageNameW(handle, 0, text.as_mut_ptr(), &mut size);
        CloseHandle(handle);
        if ok == 0 {
            return false;
        }
        std::path::PathBuf::from(String::from_utf16_lossy(&text[..size as usize]))
            .canonicalize()
            .ok()
            .as_ref()
            == Some(&executable)
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn protocol_rejects_partial_and_oversized_frames() {
        assert!(read_frame(&mut std::io::Cursor::new(b"{}".to_vec())).is_err());
        assert!(read_frame(&mut std::io::Cursor::new(vec![b'a'; MAX_LINE as usize + 1])).is_err());
        assert_eq!(
            read_frame(&mut std::io::Cursor::new(b"{}\n".to_vec()))
                .unwrap()
                .unwrap(),
            "{}\n"
        );
    }
}
