//! Build a `memory.tar.gz` archive containing session logs and MEMORY.md files.
//!
//! The archive is uploaded to GCS at session finalize time. The reconstruct
//! pipeline injects these into the Docker image for full replay fidelity.

use anyhow::{Context, Result};

use super::MemoryStorage;

/// Build a `memory.tar.gz` archive with session logs and MEMORY.md files.
pub fn build_memory_archive(storage: &MemoryStorage) -> Result<Vec<u8>> {
    use flate2::Compression;
    use flate2::write::GzEncoder;

    let buf = Vec::new();
    let enc = GzEncoder::new(buf, Compression::default());
    let mut ar = tar::Builder::new(enc);

    // Session logs
    let sessions_dir = storage.workspace_dir().join("sessions");
    if sessions_dir.is_dir() {
        for entry in std::fs::read_dir(&sessions_dir)
            .context("read sessions dir")?
            .flatten()
        {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("md") {
                let name = format!("workspace/sessions/{}", entry.file_name().to_string_lossy());
                ar.append_path_with_name(&path, &name)
                    .with_context(|| format!("archive {name}"))?;
            }
        }
    }

    // MEMORY.md files
    let global_mem = storage.global_memory_file();
    if global_mem.is_file() {
        ar.append_path_with_name(&global_mem, "global/MEMORY.md")
            .context("archive global MEMORY.md")?;
    }

    let workspace_mem = storage.workspace_memory_file();
    if workspace_mem.is_file() {
        let ws_dir_name = storage
            .workspace_dir()
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("workspace");
        let archive_path = format!("{ws_dir_name}/MEMORY.md");
        ar.append_path_with_name(&workspace_mem, &archive_path)
            .context("archive workspace MEMORY.md")?;
    }

    let enc = ar.into_inner().context("finalize tar")?;
    enc.finish().context("compress tar.gz")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn test_storage(tmp: &TempDir) -> MemoryStorage {
        let global = tmp.path().join("memory");
        let workspace = global.join("test_ws");
        MemoryStorage::with_paths(global, workspace)
    }

    #[test]
    fn test_build_empty_archive() {
        let tmp = TempDir::new().unwrap();
        let storage = test_storage(&tmp);
        let archive = build_memory_archive(&storage).unwrap();
        assert!(!archive.is_empty());
    }

    #[test]
    fn test_build_archive_with_files() {
        let tmp = TempDir::new().unwrap();
        let storage = test_storage(&tmp);
        storage.ensure_initialized().unwrap();
        storage
            .write_daily_log("2026-03-09", "test", "sess12345678", "# Test", false)
            .unwrap();

        let archive = build_memory_archive(&storage).unwrap();
        assert!(archive.len() > 100);
    }

    #[test]
    fn test_build_archive_includes_memory_md() {
        let tmp = TempDir::new().unwrap();
        let storage = test_storage(&tmp);
        storage.ensure_initialized().unwrap();

        std::fs::write(storage.global_memory_file(), "# Global Memory").unwrap();
        std::fs::write(storage.workspace_memory_file(), "# Workspace Memory").unwrap();

        let archive = build_memory_archive(&storage).unwrap();
        let entries = tar_entry_names(&archive);
        assert!(entries.contains(&"global/MEMORY.md".to_string()));
        assert!(entries.contains(&"test_ws/MEMORY.md".to_string()));
    }

    fn tar_entry_names(gz_bytes: &[u8]) -> Vec<String> {
        use flate2::read::GzDecoder;
        let decoder = GzDecoder::new(gz_bytes);
        let mut archive = tar::Archive::new(decoder);
        archive
            .entries()
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.path().unwrap().display().to_string())
            .collect()
    }
}
