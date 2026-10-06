//! Local Eclipse Theia server for Echo Code. The workbench remains a separate
//! browser application; the EchoAgent runtime and task state stay in Tauri.

use serde::Serialize;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager, State};
use uuid::Uuid;

use crate::shell_fs::FilesystemAccess;

#[derive(Default)]
pub struct TheiaServer {
    process: Mutex<Option<RunningServer>>,
    shutting_down: AtomicBool,
}

struct RunningServer {
    child: crate::process_supervisor::SyncChild,
    port: u16,
    embed_token: String,
    root: PathBuf,
}

impl RunningServer {
    fn stop(&mut self) {
        // The authenticated endpoint also triggers Theia's lifecycle on Windows,
        // where Unix signals cannot request graceful Node shutdown.
        if let Ok(mut stream) = TcpStream::connect_timeout(
            &format!("127.0.0.1:{}", self.port)
                .parse()
                .expect("loopback"),
            Duration::from_millis(200),
        ) {
            let _ = stream.set_write_timeout(Some(Duration::from_millis(200)));
            let _ = write!(stream, "POST /__echo_shutdown HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nX-Echo-Shutdown-Token: {}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n", self.port, self.embed_token);
        }
        crate::process_supervisor::stop_sync(&mut self.child);
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TheiaEndpoint {
    pub(crate) url: String,
    pub(crate) embed_token: String,
}

impl TheiaServer {
    pub fn begin_shutdown(&self) {
        self.shutting_down.store(true, Ordering::Release);
    }

    fn ensure_start_allowed(&self) -> Result<(), String> {
        if self.shutting_down.load(Ordering::Acquire) {
            Err("应用正在退出，IDE 启动已取消".into())
        } else {
            Ok(())
        }
    }

    pub fn stop(&self) {
        let mut guard = self
            .process
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(mut running) = guard.take() {
            running.stop();
        }
    }
}

impl Drop for TheiaServer {
    fn drop(&mut self) {
        self.stop();
    }
}

fn browser_app_dir(app: &AppHandle) -> Result<PathBuf, String> {
    #[cfg(debug_assertions)]
    {
        // During development, ide:stage refreshes this directory without
        // rebuilding the Tauri resource bundle next to the debug binary.
        let staged = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("resources/theia/browser");
        if staged.join("lib/backend/main.js").is_file() {
            return Ok(staged);
        }
    }
    let bundled = app
        .path()
        .resource_dir()
        .map_err(|error| format!("无法定位应用资源：{error}"))?
        .join("theia/browser");
    if bundled.join("lib/backend/main.js").is_file() {
        return Ok(bundled);
    }
    let source =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../vendor/theia-platform/examples/browser");
    if source.join("lib/backend/main.js").is_file() {
        return Ok(source);
    }
    Err("Theia 尚未构建。请先运行 pnpm ide:build。".into())
}

pub(crate) fn node_executable(app: &AppHandle) -> PathBuf {
    if let Ok(explicit) = std::env::var("ECHO_THEIA_NODE") {
        return PathBuf::from(explicit);
    }
    #[cfg(debug_assertions)]
    {
        #[cfg(target_os = "windows")]
        let staged =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("resources/theia/node/node.exe");
        #[cfg(not(target_os = "windows"))]
        let staged =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("resources/theia/node/bin/node");
        if staged.is_file() {
            return staged;
        }
    }
    if let Ok(resources) = app.path().resource_dir() {
        #[cfg(target_os = "windows")]
        let bundled = resources.join("theia/node/node.exe");
        #[cfg(not(target_os = "windows"))]
        let bundled = resources.join("theia/node/bin/node");
        if bundled.is_file() {
            return bundled;
        }
    }
    PathBuf::from("node")
}

/// Tauri's Windows resource directory can inherit a `\\?\` prefix from
/// `current_exe()`. Node.js 22/24 fails while resolving its main script from
/// that path (EISDIR on the bare drive letter), before Theia can start.
pub(crate) fn node_compatible_path(path: &Path) -> Result<PathBuf, String> {
    #[cfg(windows)]
    {
        let simplified = dunce::simplified(path);
        if simplified != path {
            return Ok(simplified.to_path_buf());
        }
        if !path.to_string_lossy().starts_with(r"\\?\") {
            return Ok(path.to_path_buf());
        }
        let Some(raw) = path.to_str() else {
            return Err(format!(
                "IDE 路径包含无法转换的 Windows 字符：{}",
                path.display()
            ));
        };
        let plain = raw.strip_prefix(r"\\?\").expect("checked above");
        let bytes = plain.as_bytes();
        let candidate = if bytes.len() >= 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && bytes[2] == b'\\'
        {
            PathBuf::from(plain)
        } else if let Some(unc) = plain.strip_prefix("UNC\\") {
            PathBuf::from(format!(r"\\{unc}"))
        } else {
            return Err(format!("IDE 不支持此 Windows 设备路径：{}", path.display()));
        };
        // dunce keeps paths over MAX_PATH in verbatim form. For a long path,
        // compare both spellings against the same real file before passing the
        // plain Unicode path to Node. This preserves long-path support on
        // Windows installations that enable it, without changing the target of
        // a path containing a reserved name or trailing space/dot.
        let same_target = matches!(
            (fs::canonicalize(path), fs::canonicalize(&candidate)),
            (Ok(original), Ok(plain)) if original == plain
        );
        if !same_target {
            return Err(format!(
                "Node.js 无法访问 IDE 路径：{}。请检查 Windows 长路径设置或改用较短的本地路径。",
                path.display()
            ));
        }
        Ok(candidate)
    }
    #[cfg(not(windows))]
    {
        Ok(path.to_path_buf())
    }
}

fn check_node(node: &PathBuf) -> Result<(), String> {
    let mut command = Command::new(node);
    command.arg("--version");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(crate::process_supervisor::WINDOWLESS_NODE_FLAGS);
    }
    let output = command
        .output()
        .map_err(|error| format!("无法启动 Theia 的 Node.js：{error}"))?;
    let version = String::from_utf8_lossy(&output.stdout);
    let major = version
        .trim()
        .trim_start_matches('v')
        .split('.')
        .next()
        .and_then(|value| value.parse::<u32>().ok())
        .unwrap_or(0);
    if !output.status.success() || !matches!(major, 22 | 24) {
        return Err(format!(
            "Theia 当前支持已验证的 Node.js 22 或 24，当前为 {}",
            version.trim()
        ));
    }
    Ok(())
}

fn theia_ready(port: u16, embed_token: &str) -> bool {
    let Ok(mut stream) = TcpStream::connect_timeout(
        &format!("127.0.0.1:{port}")
            .parse()
            .expect("valid loopback address"),
        Duration::from_millis(150),
    ) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_millis(250)));
    let _ = stream.set_write_timeout(Some(Duration::from_millis(250)));
    let request = format!(
        "GET /__echo_health?echoEmbedToken={embed_token} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"
    );
    if stream.write_all(request.as_bytes()).is_err() {
        return false;
    }
    let mut response = String::new();
    stream.read_to_string(&mut response).is_ok()
        && response.starts_with("HTTP/1.1 204")
        && response
            .lines()
            .any(|line| line.trim_end_matches('\r') == format!("X-Echo-Theia-Ready: {embed_token}"))
}

fn running_theia_ready(port: u16, embed_token: &str) -> bool {
    // A transient busy event loop should not discard the existing editor session.
    for attempt in 0..3 {
        if theia_ready(port, embed_token) {
            return true;
        }
        if attempt < 2 {
            std::thread::sleep(Duration::from_millis(150));
        }
    }
    false
}

fn port_conflict_in_attempt(log_path: &Path, attempt_log_start: usize) -> bool {
    fs::read(log_path)
        .map(|content| {
            String::from_utf8_lossy(content.get(attempt_log_start..).unwrap_or_default())
                .contains("EADDRINUSE")
        })
        .unwrap_or(false)
}

#[tauri::command]
pub async fn coding_theia_start(
    app: AppHandle,
    access: State<'_, FilesystemAccess>,
    server: State<'_, TheiaServer>,
    root: String,
) -> Result<TheiaEndpoint, String> {
    // Theia receives the selected root via the browser URL fragment. Reuse
    // EchoAgent's workspace allow-list before exposing that folder to the IDE.
    let authorized_root = access.require_workspace(&root)?;
    // The iframe must use the same ordinary spelling as Theia's file service.
    // Reject a verbatim path if removing its prefix would change its target.
    node_compatible_path(&authorized_root)?;

    let mut guard = server
        .process
        .lock()
        .map_err(|_| "Theia 状态锁不可用".to_string())?;
    server.ensure_start_allowed()?;
    if let Some(running) = guard.as_mut() {
        if running.root == authorized_root
            && running
                .child
                .try_wait()
                .map_err(|error| error.to_string())?
                .is_none()
            && running_theia_ready(running.port, &running.embed_token)
        {
            return Ok(TheiaEndpoint {
                url: format!("http://127.0.0.1:{}/", running.port),
                embed_token: running.embed_token.clone(),
            });
        }
        if let Some(mut old) = guard.take() {
            old.stop();
        }
    }
    server.ensure_start_allowed()?;

    let app_dir = node_compatible_path(&browser_app_dir(&app)?)?;
    let node = node_compatible_path(&node_executable(&app))?;
    check_node(&node)?;
    server.ensure_start_allowed()?;
    let data_dir = app
        .path()
        .app_data_dir()
        .map_err(|error| error.to_string())?;
    let config_dir = data_dir.join("theia-config");
    fs::create_dir_all(&config_dir).map_err(|error| format!("无法创建 IDE 配置目录：{error}"))?;
    let config_dir = node_compatible_path(&config_dir)?;
    let log_path = data_dir.join("theia.log");
    // Keep the failure that triggered an automatic restart available for diagnosis.
    // Rotate large logs; if preservation fails, keep appending instead of losing it.
    if fs::metadata(&log_path).is_ok_and(|meta| meta.len() >= 5 * 1024 * 1024)
        && fs::copy(&log_path, data_dir.join("theia.previous.log")).is_ok()
    {
        File::create(&log_path).map_err(|error| format!("无法轮换 IDE 日志：{error}"))?;
    }
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .map_err(|error| format!("无法创建 IDE 日志：{error}"))?;
    for attempt in 0..3 {
        server.ensure_start_allowed()?;
        // The preferred origin restores layout. A bind/drop/spawn race can
        // still occur; an occupied port gets a fresh ephemeral retry.
        let listener = if attempt == 0 {
            TcpListener::bind("127.0.0.1:41773").or_else(|_| TcpListener::bind("127.0.0.1:0"))
        } else {
            TcpListener::bind("127.0.0.1:0")
        }
        .map_err(|error| format!("无法分配 Theia 本地端口：{error}"))?;
        let port = listener
            .local_addr()
            .map_err(|error| error.to_string())?
            .port();
        drop(listener);

        let embed_token = format!("{}{}", Uuid::now_v7().simple(), Uuid::now_v7().simple());
        let mut log = OpenOptions::new()
            .append(true)
            .open(&log_path)
            .map_err(|error| format!("无法打开 IDE 日志：{error}"))?;
        let attempt_log_start = log.metadata().map_err(|error| error.to_string())?.len() as usize;
        writeln!(
            log,
            "\n--- Theia 启动尝试 {}，端口 {} ---",
            attempt + 1,
            port
        )
        .map_err(|error| format!("无法写入 IDE 日志：{error}"))?;
        let err_log = log.try_clone().map_err(|error| error.to_string())?;
        let mut command = Command::new(&node);
        command
            .arg(app_dir.join("lib/backend/main.js"))
            .arg(format!("--port={port}"))
            .arg("--hostname=127.0.0.1")
            .arg("--plugins=local-dir:plugins")
            .env("THEIA_CONFIG_DIR", &config_dir)
            .env("ECHO_THEIA_EMBED_TOKEN", &embed_token)
            .current_dir(&app_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::from(log))
            .stderr(Stdio::from(err_log));
        let mut child = crate::process_supervisor::spawn_sync(command)
            .map_err(|error| format!("Theia 启动失败：{error}"))?;

        let deadline = Instant::now() + Duration::from_secs(30);
        while Instant::now() < deadline {
            if let Err(error) = server.ensure_start_allowed() {
                crate::process_supervisor::stop_sync(&mut child);
                return Err(error);
            }
            if let Some(status) = child.try_wait().map_err(|error| error.to_string())? {
                let port_taken = port_conflict_in_attempt(&log_path, attempt_log_start);
                if port_taken && attempt < 2 {
                    break;
                }
                return Err(format!(
                    "Theia 启动后退出：{status}。日志：{}",
                    log_path.display()
                ));
            }
            if theia_ready(port, &embed_token) {
                if let Err(error) = server.ensure_start_allowed() {
                    crate::process_supervisor::stop_sync(&mut child);
                    return Err(error);
                }
                *guard = Some(RunningServer {
                    child,
                    port,
                    embed_token: embed_token.clone(),
                    root: authorized_root,
                });
                return Ok(TheiaEndpoint {
                    url: format!("http://127.0.0.1:{port}/"),
                    embed_token,
                });
            }
            std::thread::sleep(Duration::from_millis(150));
        }
        if child
            .try_wait()
            .map_err(|error| error.to_string())?
            .is_none()
        {
            crate::process_supervisor::stop_sync(&mut child);
            return Err(format!(
                "Theia 启动超时，请查看日志：{}",
                log_path.display()
            ));
        }
    }
    Err(format!(
        "Theia 本地端口持续被占用，请查看日志：{}",
        log_path.display()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn port_conflict_checks_only_the_current_start_attempt() {
        let directory = tempfile::tempdir().unwrap();
        let log_path = directory.path().join("theia.log");
        fs::write(&log_path, b"first attempt: EADDRINUSE\n").unwrap();
        let second_attempt_offset = fs::metadata(&log_path).unwrap().len() as usize;
        let mut log = OpenOptions::new().append(true).open(&log_path).unwrap();
        writeln!(log, "second attempt: missing module").unwrap();
        assert!(!port_conflict_in_attempt(&log_path, second_attempt_offset));
        assert!(port_conflict_in_attempt(&log_path, 0));
    }

    #[cfg(windows)]
    #[test]
    fn node_entry_uses_a_regular_windows_path() {
        let path = Path::new(r"\\?\C:\Program Files\EchoAgent\theia\browser");
        assert_eq!(
            node_compatible_path(path).unwrap(),
            PathBuf::from(r"C:\Program Files\EchoAgent\theia\browser")
        );
        let chinese = Path::new(r"\\?\C:\应用\代码开发\theia\browser");
        assert_eq!(
            node_compatible_path(chinese).unwrap(),
            PathBuf::from(r"C:\应用\代码开发\theia\browser")
        );
    }

    #[cfg(windows)]
    #[test]
    fn node_entry_keeps_long_unicode_paths_when_windows_can_resolve_them() {
        let directory = tempfile::tempdir().unwrap();
        let base = directory.path().canonicalize().unwrap();
        let long_dir = base.join("中文路径".repeat(30)).join("代码开发".repeat(30));
        fs::create_dir_all(&long_dir).unwrap();
        let entry = long_dir.join("入口.js");
        fs::write(&entry, "process.stdout.write('ok')").unwrap();
        let plain = PathBuf::from(entry.to_str().unwrap().strip_prefix(r"\\?\").unwrap());
        assert!(plain.to_string_lossy().encode_utf16().count() > 260);

        let require_long_path = std::env::var_os("ECHO_VALIDATE_LONG_PATHS").as_deref()
            == Some(std::ffi::OsStr::new("1"));
        let plain_is_resolvable = plain.canonicalize().is_ok();
        if require_long_path {
            assert!(
                plain_is_resolvable,
                "Windows must resolve ordinary long Unicode paths"
            );
        }
        if plain_is_resolvable {
            assert_eq!(node_compatible_path(&entry).unwrap(), plain);
            let staged_node =
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("resources/theia/node/node.exe");
            if require_long_path {
                assert!(
                    staged_node.is_file(),
                    "stage the packaged Node.js before this test"
                );
            }
            if staged_node.is_file() {
                let output = Command::new(staged_node).arg(&plain).output().unwrap();
                assert!(
                    output.status.success(),
                    "{}",
                    String::from_utf8_lossy(&output.stderr)
                );
                assert_eq!(output.stdout.as_slice(), b"ok");
            }
        } else {
            assert!(node_compatible_path(&entry).is_err());
        }
    }
}
