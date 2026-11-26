use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};

pub(super) fn open_directory_path(path: &Path) -> Option<File> {
    use std::os::windows::fs::OpenOptionsExt as _;

    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    let mut options = OpenOptions::new();
    options
        .read(true)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT);
    options.open(path).ok()
}

pub(super) fn open_regular_path(path: &Path) -> Option<File> {
    use std::os::windows::fs::OpenOptionsExt as _;

    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    let mut options = OpenOptions::new();
    options
        .read(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    options.open(path).ok()
}

pub(super) fn directory_path_matches(path: &Path, expected: &File) -> bool {
    let Some(opened) = open_directory_path(path) else {
        return false;
    };
    same_open_file(expected, &opened) && final_handle_path_matches(path, expected)
}

pub(super) fn canonical_file_matches(path: &Path, expected: &File) -> bool {
    let Ok(canonical) = dunce::canonicalize(path) else {
        return false;
    };
    if canonical != path {
        return false;
    }
    let Some(opened) = open_regular_path(path) else {
        return false;
    };
    same_open_file(expected, &opened) && final_handle_path_matches(path, expected)
}

pub(super) fn same_open_file(expected: &File, opened: &File) -> bool {
    file_identity(expected)
        .is_some_and(|expected| file_identity(opened).is_some_and(|opened| opened == expected))
}

pub(super) fn final_handle_path_matches(path: &Path, file: &File) -> bool {
    use std::os::windows::io::AsRawHandle as _;

    final_path_from_raw_handle(file.as_raw_handle().cast()).is_some_and(|handle_path| {
        dunce::canonicalize(path).is_ok_and(|path| path == dunce::simplified(&handle_path))
    })
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct FileIdentity {
    volume_serial_number: u32,
    file_index: u64,
}

#[repr(C)]
#[allow(dead_code)]
struct FileTime {
    low: u32,
    high: u32,
}

#[repr(C)]
#[allow(dead_code)]
struct ByHandleFileInformation {
    file_attributes: u32,
    creation_time: FileTime,
    last_access_time: FileTime,
    last_write_time: FileTime,
    volume_serial_number: u32,
    file_size_high: u32,
    file_size_low: u32,
    number_of_links: u32,
    file_index_high: u32,
    file_index_low: u32,
}

fn file_identity(file: &File) -> Option<FileIdentity> {
    use std::os::windows::io::AsRawHandle as _;

    raw_handle_identity(file.as_raw_handle().cast())
}

fn raw_handle_identity(handle: *mut std::ffi::c_void) -> Option<FileIdentity> {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        #[link_name = "GetFileInformationByHandle"]
        fn get_file_information_by_handle(
            file: *mut std::ffi::c_void,
            information: *mut ByHandleFileInformation,
        ) -> i32;
    }

    let mut information = std::mem::MaybeUninit::uninit();
    // SAFETY: the borrowed handle is live and the output has the exact
    // BY_HANDLE_FILE_INFORMATION layout.
    let result = unsafe { get_file_information_by_handle(handle, information.as_mut_ptr()) };
    if result == 0 {
        return None;
    }
    // SAFETY: a nonzero result initializes the entire output structure.
    let information = unsafe { information.assume_init() };
    Some(FileIdentity {
        volume_serial_number: information.volume_serial_number,
        file_index: (u64::from(information.file_index_high) << 32)
            | u64::from(information.file_index_low),
    })
}

fn final_path_from_raw_handle(handle: *mut std::ffi::c_void) -> Option<PathBuf> {
    use std::os::windows::ffi::OsStringExt as _;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        #[link_name = "GetFinalPathNameByHandleW"]
        fn get_final_path_name_by_handle(
            file: *mut std::ffi::c_void,
            path: *mut u16,
            path_len: u32,
            flags: u32,
        ) -> u32;
    }

    // SAFETY: a null output with length zero is the documented size query.
    let needed = unsafe { get_final_path_name_by_handle(handle, std::ptr::null_mut(), 0, 0) };
    if needed == 0 {
        return None;
    }
    let mut buffer = vec![0_u16; needed as usize + 1];
    // SAFETY: `buffer` has the advertised writable UTF-16 capacity.
    let written = unsafe {
        get_final_path_name_by_handle(
            handle,
            buffer.as_mut_ptr(),
            u32::try_from(buffer.len()).unwrap_or(u32::MAX),
            0,
        )
    };
    if written == 0 || written as usize >= buffer.len() {
        return None;
    }
    Some(PathBuf::from(OsString::from_wide(
        &buffer[..written as usize],
    )))
}

#[cfg(test)]
pub(super) fn same_file_for_test(expected: &File, opened: &File) -> bool {
    same_open_file(expected, opened)
}

#[cfg(test)]
pub(super) fn final_path_matches_for_test(path: &Path, file: &File) -> bool {
    final_handle_path_matches(path, file)
}
