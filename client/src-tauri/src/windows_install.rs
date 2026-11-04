//! Windows install locations must survive cleanup of temporary files.
//!
//! Keep the lexical policy executable on every host; Windows packaging tests
//! additionally exercise the installer, registry and shortcuts on a real VM.

fn normalize(path: &str, allow_root: bool) -> Option<String> {
    if path
        .chars()
        .any(|c| c.is_control() || matches!(c, '"' | '<' | '>' | '|' | '*' | '?'))
    {
        return None;
    }
    let path = path.replace('/', "\\");
    let (prefix, rest) = if path.as_bytes().get(1) == Some(&b':')
        && path.as_bytes()[0].is_ascii_alphabetic()
        && path.as_bytes().get(2) == Some(&b'\\')
    {
        (path[..2].to_lowercase(), &path[3..])
    } else if let Some(rest) = path.strip_prefix("\\\\") {
        let mut components = rest.splitn(3, '\\');
        let server = components.next()?;
        let share = components.next()?;
        if server.is_empty() || share.is_empty() || matches!(server, "." | "..") {
            return None;
        }
        (
            format!("\\\\{}\\{}", server.to_lowercase(), share.to_lowercase()),
            components.next().unwrap_or(""),
        )
    } else {
        return None;
    };
    let mut parts = Vec::new();
    for part in rest.split('\\') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            part if part.contains(':') || part.ends_with(' ') || part.ends_with('.') => {
                return None
            }
            part => parts.push(part.to_lowercase()),
        }
    }
    if parts.is_empty() {
        return allow_root.then_some(prefix);
    }
    Some(format!("{}\\{}", prefix, parts.join("\\")))
}

fn within(path: &str, root: &str) -> bool {
    path == root
        || path
            .strip_prefix(root)
            .is_some_and(|tail| tail.starts_with('\\'))
}

pub(crate) fn durable_install_dir(
    current: &str,
    default: &str,
    disposable_roots: &[String],
) -> Result<String, String> {
    let roots: Vec<_> = disposable_roots
        .iter()
        .filter_map(|root| normalize(root, true))
        .collect();
    let durable = |path: &str| {
        normalize(path, false).filter(|path| !roots.iter().any(|root| within(path, root)))
    };
    if durable(current).is_some() {
        return Ok(current.into());
    }
    if durable(default).is_some() {
        return Ok(default.into());
    }
    Err("无法确定可靠的安装目录。请重新运行安装包，选择临时目录以外的文件夹。".into())
}

#[cfg(windows)]
pub(crate) fn updater_executable() -> Result<std::path::PathBuf, String> {
    let executable =
        std::env::current_exe().map_err(|error| format!("读取当前安装位置失败：{error}"))?;
    // canonicalize expands junctions and 8.3 aliases before classifying a path.
    let executable = dunce::canonicalize(executable)
        .map_err(|error| format!("确认当前安装位置失败：{error}"))?;
    let current = executable
        .parent()
        .and_then(|path| path.to_str())
        .ok_or("安装目录不是有效的 Windows 路径")?;
    let local = dirs::data_local_dir().ok_or("无法读取当前用户的应用安装目录")?;
    let default = local.join("EchoAgent");
    let mut roots = vec![
        std::env::temp_dir().to_string_lossy().into_owned(),
        local.join("Temp").to_string_lossy().into_owned(),
        local
            .join("Microsoft\\Windows\\INetCache")
            .to_string_lossy()
            .into_owned(),
    ];
    for name in ["TEMP", "TMP"] {
        if let Ok(value) = std::env::var(name) {
            roots.push(value);
        }
    }
    if let Some(windows) = std::env::var_os("WINDIR") {
        roots.push(
            std::path::PathBuf::from(windows)
                .join("Temp")
                .to_string_lossy()
                .into_owned(),
        );
    }
    // Match the resolved executable against resolved roots too (TEMP itself
    // can be a junction or an 8.3 alias).
    let roots: Vec<_> = roots
        .into_iter()
        .map(|root| {
            dunce::canonicalize(&root)
                .ok()
                .and_then(|path| path.to_str().map(str::to_owned))
                .unwrap_or(root)
        })
        .collect();
    let target = durable_install_dir(
        current,
        default.to_str().ok_or("默认安装目录无法编码")?,
        &roots,
    )?;
    Ok(std::path::PathBuf::from(target).join(executable.file_name().ok_or("当前程序没有文件名")?))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn target(current: &str) -> String {
        durable_install_dir(
            current,
            r"C:\Users\用户\AppData\Local\EchoAgent",
            &[
                r"C:\Users\用户\AppData\Local\Temp".into(),
                r"C:\Windows\Temp".into(),
                r"D:\缓存".into(),
            ],
        )
        .unwrap()
    }
    #[test]
    fn repaired_locations_survive_temp_cleanup() {
        for path in [
            r"C:\Users\用户\AppData\Local\Temp\EchoAgentInstallTest",
            r"c:/USERS/用户/AppData/Local/temp/sub/../EchoAgent",
            r"C:\Windows\TEMP\EchoAgent",
            r"D:\缓存\EchoAgent",
        ] {
            assert_eq!(target(path), r"C:\Users\用户\AppData\Local\EchoAgent");
        }
    }
    #[test]
    fn custom_stable_unicode_and_network_locations_are_preserved() {
        for path in [
            r"D:\应用程序\Echo Agent",
            r"C:\Program Files\EchoAgent",
            r"D:\缓存库\EchoAgent",
            r"\\server\apps\EchoAgent",
        ] {
            assert_eq!(target(path), path);
        }
    }
    #[test]
    fn rejects_relative_roots_device_paths_and_command_injection() {
        for path in [
            "EchoAgent",
            r"C:\",
            r"\\server\apps",
            r"\\?\C:\App",
            r"\\.\pipe\app",
            "C:\\App\" /S",
            "C:\\App\n/D=D:\\Other",
            r"C:\App.\Echo",
            r"C:\..\App",
        ] {
            assert_ne!(target(path), path);
        }
    }
    #[test]
    fn invalid_fallback_cannot_reintroduce_a_temporary_install() {
        assert!(
            durable_install_dir(r"C:\Temp\old", r"C:\Temp\EchoAgent", &[r"C:\Temp".into()])
                .is_err()
        );
        assert_eq!(
            durable_install_dir(r"D:\App", r"C:\Local\EchoAgent", &[r"D:\".into()]).unwrap(),
            r"C:\Local\EchoAgent"
        );
    }
}
