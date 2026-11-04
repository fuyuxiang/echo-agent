// Copyright 2026 EchoAgent contributors. SPDX-License-Identifier: MIT
use std::{ffi::OsString, io, path::Path};

/// NSIS treats /D= specially: it must be the last argument and must not be
/// quoted, even for paths with spaces. Validate before constructing that tail.
pub(crate) fn install_dir_arg(directory: &Path) -> io::Result<OsString> {
    let value = directory.to_str().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "Invalid NSIS install directory encoding",
        )
    })?;
    if value.is_empty() || value.chars().any(|c| c.is_control() || c == '"') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Invalid NSIS install directory",
        ));
    }
    Ok(OsString::from(format!("/D={value}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preserves_spaces_and_unicode_without_quotes() {
        assert_eq!(
            install_dir_arg(Path::new(r"D:\应用程序\Echo Agent")).unwrap(),
            r"/D=D:\应用程序\Echo Agent"
        );
    }
    #[test]
    fn rejects_switch_injection() {
        for value in ["", "C:\\App\" /S", "C:\\App\n/D=D:\\Other"] {
            assert!(install_dir_arg(Path::new(value)).is_err());
        }
    }
}
