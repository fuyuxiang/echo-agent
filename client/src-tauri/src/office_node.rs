//! Node path handling for the packaged Office worker.
#[cfg(windows)]
use std::fs;
use std::path::{Path, PathBuf};

pub(crate) const NODE_RELATIVE: &str = if cfg!(windows) {
    "node/node.exe"
} else {
    "node/bin/node"
};

/// Tauri's Windows resource directory can inherit a `\\?\` prefix from
/// `current_exe()`. Node.js 22/24 fails while resolving its main script from
/// that path (EISDIR on the bare drive letter), before the Office worker can start.
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
                "办公运行时路径包含无法转换的 Windows 字符：{}",
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
            return Err(format!(
                "办公运行时不支持此 Windows 设备路径：{}",
                path.display()
            ));
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
                "Node.js 无法访问办公运行时路径：{}。请检查 Windows 长路径设置或改用较短的本地路径。",
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

pub(crate) fn node_compatible_output_path(path: &Path) -> Result<PathBuf, String> {
    #[cfg(windows)]
    {
        // The worker generates this file inside a newly created private directory.
        // Resolve the existing directory before adding the generated filename.
        let parent = path.parent().ok_or("办公输出缺少保存目录")?;
        let name = path.file_name().ok_or("办公输出缺少文件名")?;
        Ok(node_compatible_path(parent)?.join(name))
    }
    #[cfg(not(windows))]
    {
        Ok(path.to_owned())
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use std::process::Command;

    #[cfg(windows)]
    #[test]
    fn node_entry_uses_a_regular_windows_path() {
        let path = Path::new(r"\\?\C:\Program Files\EchoAgent\office");
        assert_eq!(
            node_compatible_path(path).unwrap(),
            PathBuf::from(r"C:\Program Files\EchoAgent\office")
        );
        let chinese = Path::new(r"\\?\C:\应用\办公文档\office");
        assert_eq!(
            node_compatible_path(chinese).unwrap(),
            PathBuf::from(r"C:\应用\办公文档\office")
        );
    }

    #[cfg(windows)]
    #[test]
    fn node_entry_keeps_long_unicode_paths_when_windows_can_resolve_them() {
        let directory = tempfile::tempdir().unwrap();
        let base = directory.path().canonicalize().unwrap();
        let long_dir = base.join("中文路径".repeat(30)).join("办公文档".repeat(30));
        fs::create_dir_all(&long_dir).unwrap();
        let entry = long_dir.join("入口.js");
        let output = long_dir.join("新建成果.docx");
        fs::write(
            &entry,
            "require('node:fs').writeFileSync(process.argv[2], 'ok')",
        )
        .unwrap();
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
            assert!(!output.exists());
            let node_output = node_compatible_output_path(&output).unwrap();
            assert_eq!(node_output.parent(), plain.parent());
            let staged_node =
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("resources/office/node/node.exe");
            if require_long_path {
                assert!(
                    staged_node.is_file(),
                    "stage the packaged Node.js before this test"
                );
            }
            if staged_node.is_file() {
                let process = Command::new(staged_node)
                    .arg(&plain)
                    .arg(&node_output)
                    .output()
                    .unwrap();
                assert!(
                    process.status.success(),
                    "{}",
                    String::from_utf8_lossy(&process.stderr)
                );
                assert_eq!(fs::read(&output).unwrap(), b"ok");
            }
        } else {
            assert!(node_compatible_path(&entry).is_err());
        }
    }
}
