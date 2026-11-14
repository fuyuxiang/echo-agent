//! Bounded Windows application discovery. Registry values are paths only,
//! never shell commands, arguments, or model-supplied launch instructions.
use std::path::{Path, PathBuf};

pub fn roots(env: impl Fn(&str) -> Option<std::ffi::OsString>) -> Vec<(PathBuf, usize)> {
    let mut roots = vec![];
    if let Some(root) = env("SystemRoot").or_else(|| env("WINDIR")) {
        let root = PathBuf::from(root);
        // Do not recurse through WinSxS, servicing, drivers or system caches.
        roots.push((root.join("System32"), 0));
        roots.push((root, 0));
    }
    for name in ["ProgramFiles", "ProgramFiles(x86)", "LOCALAPPDATA"] {
        if let Some(path) = env(name) {
            roots.push((PathBuf::from(path), 4));
        }
    }
    roots
}

pub fn executable_path(value: &str) -> Option<PathBuf> {
    let value = value.trim();
    let value = value
        .strip_prefix('"')
        .and_then(|v| v.strip_suffix('"'))
        .unwrap_or(value);
    let path = PathBuf::from(value);
    (path.is_absolute()
        && !value.chars().any(char::is_control)
        && !value.contains('"')
        && path
            .extension()
            .is_some_and(|v| v.eq_ignore_ascii_case("exe")))
    .then_some(path)
}

/// System directories contain many console utilities. Include real GUI PE
/// executables without consuming the catalog budget on command-line helpers.
pub fn is_gui(path: &Path) -> bool {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut file) = std::fs::File::open(path) else {
        return false;
    };
    let mut dos = [0u8; 64];
    if file.read_exact(&mut dos).is_err() || &dos[..2] != b"MZ" {
        return false;
    }
    let offset = u32::from_le_bytes(dos[60..64].try_into().unwrap());
    if offset > 1024 * 1024 || file.seek(SeekFrom::Start(offset.into())).is_err() {
        return false;
    }
    let mut header = [0u8; 94];
    file.read_exact(&mut header).is_ok()
        && &header[..4] == b"PE\0\0"
        && matches!(u16::from_le_bytes([header[24], header[25]]), 0x10b | 0x20b)
        && u16::from_le_bytes([header[92], header[93]]) == 2
}

#[cfg(target_os = "windows")]
pub fn registered_paths() -> Vec<PathBuf> {
    use windows_sys::Win32::{
        Foundation::{ERROR_NO_MORE_ITEMS, ERROR_SUCCESS},
        System::Registry::*,
    };
    struct Key(HKEY);
    impl Drop for Key {
        fn drop(&mut self) {
            unsafe {
                RegCloseKey(self.0);
            }
        }
    }
    let mut paths = vec![];
    let name: Vec<_> = "Software\\Microsoft\\Windows\\CurrentVersion\\App Paths"
        .encode_utf16()
        .chain(Some(0))
        .collect();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    for hive in [HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE] {
        for view in [KEY_WOW64_64KEY, KEY_WOW64_32KEY] {
            let mut key = std::ptr::null_mut();
            if unsafe { RegOpenKeyExW(hive, name.as_ptr(), 0, KEY_READ | view, &mut key) }
                != ERROR_SUCCESS
            {
                continue;
            }
            let key = Key(key);
            for index in 0..1024 {
                if std::time::Instant::now() >= deadline || paths.len() >= 512 {
                    return paths;
                }
                let mut child = [0u16; 512];
                let mut length = child.len() as u32;
                let result = unsafe {
                    RegEnumKeyExW(
                        key.0,
                        index,
                        child.as_mut_ptr(),
                        &mut length,
                        std::ptr::null(),
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                    )
                };
                if result == ERROR_NO_MORE_ITEMS {
                    break;
                }
                if result != ERROR_SUCCESS || length as usize >= child.len() {
                    continue;
                }
                child[length as usize] = 0;
                let mut value = [0u16; 4096];
                let mut bytes = std::mem::size_of_val(&value) as u32;
                // RegGetValue expands REG_EXPAND_SZ with the host environment.
                let result = unsafe {
                    RegGetValueW(
                        key.0,
                        child.as_ptr(),
                        std::ptr::null(),
                        RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ,
                        std::ptr::null_mut(),
                        value.as_mut_ptr().cast(),
                        &mut bytes,
                    )
                };
                if result != ERROR_SUCCESS
                    || bytes == 0
                    || bytes as usize > std::mem::size_of_val(&value)
                    || bytes % 2 != 0
                {
                    continue;
                }
                let length = value[..bytes as usize / 2]
                    .iter()
                    .position(|v| *v == 0)
                    .unwrap_or(bytes as usize / 2);
                if let Ok(value) = String::from_utf16(&value[..length]) {
                    if let Some(path) = executable_path(&value) {
                        paths.push(path);
                    }
                }
            }
        }
    }
    paths
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn windows_system_roots_are_shallow_and_precede_program_caches() {
        let root = std::env::temp_dir().join("catalog-windows");
        let roots = roots(|key| match key {
            "WINDIR" => Some(root.clone().into_os_string()),
            "LOCALAPPDATA" => Some(root.join("user-apps").into_os_string()),
            _ => None,
        });
        assert_eq!(
            roots,
            vec![
                (root.join("System32"), 0),
                (root.clone(), 0),
                (root.join("user-apps"), 4)
            ]
        );
    }
    #[test]
    fn registry_paths_accept_spaces_and_quotes_but_never_arguments() {
        let path = std::env::temp_dir()
            .join("Program Files")
            .join("Example.exe");
        assert_eq!(
            executable_path(&format!("\"{}\"", path.display())),
            Some(path.clone())
        );
        for value in [
            "relative.exe".into(),
            format!("\"{}\" --command", path.display()),
            format!("{} --command", path.display()),
            format!("{}\n", path.display()).replace(".exe\n", "\n.exe"),
        ] {
            assert!(executable_path(&value).is_none(), "{value}");
        }
    }
    #[test]
    fn system_gui_detection_checks_actual_pe_subsystem() {
        let path = std::env::temp_dir().join(format!("echo-catalog-{}.exe", uuid::Uuid::now_v7()));
        let mut bytes = vec![0u8; 64 + 94];
        bytes[..2].copy_from_slice(b"MZ");
        bytes[60..64].copy_from_slice(&64u32.to_le_bytes());
        bytes[64..68].copy_from_slice(b"PE\0\0");
        bytes[88..90].copy_from_slice(&0x20bu16.to_le_bytes());
        bytes[156..158].copy_from_slice(&2u16.to_le_bytes());
        std::fs::write(&path, &bytes).unwrap();
        assert!(is_gui(&path));
        bytes[156..158].copy_from_slice(&3u16.to_le_bytes());
        std::fs::write(&path, bytes).unwrap();
        assert!(!is_gui(&path));
        std::fs::remove_file(path).unwrap();
    }
}
