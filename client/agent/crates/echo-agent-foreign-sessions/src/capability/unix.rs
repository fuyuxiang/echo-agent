use std::ffi::{CStr, OsStr, OsString};
use std::fs::{File, OpenOptions};
use std::path::Path;

use super::DirectoryVisit;

pub(super) fn open_directory_path(path: &Path) -> Option<File> {
    use std::os::unix::fs::OpenOptionsExt as _;

    let mut options = OpenOptions::new();
    options
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK);
    options.open(path).ok()
}

pub(super) fn open_directory_relative(directory: &File, path: &Path) -> Option<File> {
    let mut current = directory.try_clone().ok()?;
    for component in path.components() {
        let std::path::Component::Normal(name) = component else {
            continue;
        };
        current = openat_component(
            &current,
            name,
            libc::O_RDONLY
                | libc::O_DIRECTORY
                | libc::O_CLOEXEC
                | libc::O_NOFOLLOW
                | libc::O_NONBLOCK,
        )?;
    }
    Some(current)
}

pub(super) fn open_regular_relative(directory: &File, path: &Path) -> Option<File> {
    let parent = path.parent().unwrap_or_else(|| Path::new(""));
    let directory = open_directory_relative(directory, parent)?;
    openat_component(
        &directory,
        path.file_name()?,
        libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
    )
}

fn openat_component(directory: &File, name: &OsStr, flags: i32) -> Option<File> {
    use std::os::fd::{AsRawFd as _, FromRawFd as _};
    use std::os::unix::ffi::OsStrExt as _;

    let name = std::ffi::CString::new(name.as_bytes()).ok()?;
    // SAFETY: the directory fd and NUL-terminated child name are valid; no
    // creation mode argument is required because the flags never create.
    let fd = unsafe { libc::openat(directory.as_raw_fd(), name.as_ptr(), flags) };
    // SAFETY: a nonnegative `openat` result transfers one owned fd.
    (fd >= 0).then(|| unsafe { File::from_raw_fd(fd) })
}

struct DirectoryStream(*mut libc::DIR);

impl DirectoryStream {
    fn open(directory: &File) -> Option<Self> {
        use std::os::fd::AsRawFd as _;

        let dot = c".";
        // SAFETY: opening "." relative to a live directory fd returns an
        // independent directory description for `fdopendir` to own.
        let fd = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                dot.as_ptr(),
                libc::O_RDONLY
                    | libc::O_DIRECTORY
                    | libc::O_CLOEXEC
                    | libc::O_NOFOLLOW
                    | libc::O_NONBLOCK,
            )
        };
        if fd < 0 {
            return None;
        }
        // SAFETY: `fd` is an owned directory fd and ownership passes to DIR.
        let stream = unsafe { libc::fdopendir(fd) };
        if stream.is_null() {
            // SAFETY: ownership did not transfer when `fdopendir` failed.
            unsafe {
                libc::close(fd);
            }
            return None;
        }
        Some(Self(stream))
    }

    fn next(&mut self) -> Result<Option<OsString>, ()> {
        use std::os::unix::ffi::OsStringExt as _;

        loop {
            set_errno(0);
            // SAFETY: the stream remains owned and live until close/drop.
            let entry = unsafe { libc::readdir(self.0) };
            if entry.is_null() {
                return if errno() == 0 { Ok(None) } else { Err(()) };
            }
            // SAFETY: POSIX guarantees a NUL-terminated d_name for this entry.
            let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
            if name != b"." && name != b".." {
                return Ok(Some(OsString::from_vec(name.to_vec())));
            }
        }
    }

    fn close(mut self) -> bool {
        let stream = std::mem::replace(&mut self.0, std::ptr::null_mut());
        // SAFETY: this is the one explicit close for the owned DIR stream.
        unsafe { libc::closedir(stream) == 0 }
    }
}

impl Drop for DirectoryStream {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: Drop owns the stream only when explicit close did not.
            unsafe {
                libc::closedir(self.0);
            }
        }
    }
}

pub(super) fn visit_directory_names(directory: &File, mut visit: impl FnMut(OsString)) -> bool {
    let Some(mut stream) = DirectoryStream::open(directory) else {
        return false;
    };
    let complete = loop {
        match stream.next() {
            Ok(Some(name)) => visit(name),
            Ok(None) => break true,
            Err(()) => break false,
        }
    };
    stream.close() && complete
}

pub(super) fn visit_directory_names_bounded(
    directory: &File,
    max_entries: usize,
    mut visit: impl FnMut(OsString),
) -> DirectoryVisit {
    let Some(mut stream) = DirectoryStream::open(directory) else {
        return DirectoryVisit {
            visited: 0,
            complete: false,
        };
    };
    let mut visited = 0;
    let complete = loop {
        if visited == max_entries {
            break matches!(stream.next(), Ok(None));
        }
        match stream.next() {
            Ok(Some(name)) => {
                visited += 1;
                visit(name);
            }
            Ok(None) => break true,
            Err(()) => break false,
        }
    };
    DirectoryVisit {
        visited,
        complete: stream.close() && complete,
    }
}

#[cfg(any(target_os = "linux", target_os = "dragonfly"))]
fn errno() -> i32 {
    // SAFETY: libc exposes one thread-local errno cell for the current thread.
    unsafe { *libc::__errno_location() }
}

#[cfg(any(target_os = "linux", target_os = "dragonfly"))]
fn set_errno(value: i32) {
    // SAFETY: libc exposes one thread-local errno cell for the current thread.
    unsafe {
        *libc::__errno_location() = value;
    }
}

#[cfg(any(target_os = "macos", target_os = "ios", target_os = "freebsd"))]
fn errno() -> i32 {
    // SAFETY: libc exposes one thread-local errno cell for the current thread.
    unsafe { *libc::__error() }
}

#[cfg(any(target_os = "macos", target_os = "ios", target_os = "freebsd"))]
fn set_errno(value: i32) {
    // SAFETY: libc exposes one thread-local errno cell for the current thread.
    unsafe {
        *libc::__error() = value;
    }
}

#[cfg(any(target_os = "android", target_os = "netbsd", target_os = "openbsd"))]
fn errno() -> i32 {
    // SAFETY: libc exposes one thread-local errno cell for the current thread.
    unsafe { *libc::__errno() }
}

#[cfg(any(target_os = "android", target_os = "netbsd", target_os = "openbsd"))]
fn set_errno(value: i32) {
    // SAFETY: libc exposes one thread-local errno cell for the current thread.
    unsafe {
        *libc::__errno() = value;
    }
}

#[cfg(not(any(
    target_os = "linux",
    target_os = "android",
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd",
    target_os = "dragonfly",
    target_os = "netbsd",
    target_os = "openbsd"
)))]
fn errno() -> i32 {
    1
}

#[cfg(not(any(
    target_os = "linux",
    target_os = "android",
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd",
    target_os = "dragonfly",
    target_os = "netbsd",
    target_os = "openbsd"
)))]
fn set_errno(_value: i32) {}
