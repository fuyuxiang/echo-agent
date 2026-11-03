#![allow(dead_code)]
mod paths {
    pub(crate) fn harden_private_dir(_: &std::path::Path) -> Result<(), String> { Ok(()) }
    pub(crate) fn harden_private_file(_: &std::path::Path) -> Result<(), String> { Ok(()) }
}
mod shell_fs {
    pub(crate) fn read_regular_file_bounded(path: &std::path::Path, max: u64) -> Result<Vec<u8>, String> {
        let meta = std::fs::symlink_metadata(path).map_err(|error| error.to_string())?;
        if !meta.is_file() || meta.file_type().is_symlink() || meta.len() > max { return Err("文件类型或大小无效".into()); }
        std::fs::read(path).map_err(|error| error.to_string())
    }
}
#[path = "../../src-tauri/src/document_store.rs"]
mod document_store;
