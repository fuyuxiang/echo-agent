pub mod ripgrep;

use std::io;

/// True if `e` reports that an advisory `flock` is held by another process.
/// Unix surfaces this as `WouldBlock`; Windows as `ERROR_LOCK_VIOLATION` (OS
/// error 33), matched via [`fs2::lock_contended_error`].
pub fn is_lock_contended(e: &io::Error) -> bool {
    e.kind() == io::ErrorKind::WouldBlock
        || (e.raw_os_error().is_some()
            && e.raw_os_error() == fs2::lock_contended_error().raw_os_error())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_lock_contended_classifies_errors() {
        assert!(is_lock_contended(&fs2::lock_contended_error()));
        assert!(is_lock_contended(&io::Error::new(
            io::ErrorKind::WouldBlock,
            "would block"
        )));
        #[cfg(windows)]
        assert!(is_lock_contended(&io::Error::from_raw_os_error(33)));
        assert!(!is_lock_contended(&io::Error::new(
            io::ErrorKind::PermissionDenied,
            "denied"
        )));
    }
}
