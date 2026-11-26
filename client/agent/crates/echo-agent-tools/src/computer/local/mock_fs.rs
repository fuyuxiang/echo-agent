//! Mock file system implementation for testing.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::computer::types::{AsyncFileSystem, ComputerError};

/// In-memory file system for testing.
/// Thread-safe and async-compatible.
pub struct MockFs {
    files: Arc<RwLock<HashMap<PathBuf, Vec<u8>>>>,
}

impl Default for MockFs {
    fn default() -> Self {
        Self::new()
    }
}

impl MockFs {
    /// Create a new empty mock file system.
    pub fn new() -> Self {
        Self {
            files: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Set a file's contents directly (for test setup).
    pub async fn set_file(&self, path: impl AsRef<Path>, content: &[u8]) {
        self.files
            .write()
            .await
            .insert(path.as_ref().to_path_buf(), content.to_vec());
    }

    /// Get a file's contents directly (for test assertions).
    pub async fn get_file(&self, path: impl AsRef<Path>) -> Option<Vec<u8>> {
        self.files.read().await.get(path.as_ref()).cloned()
    }

    /// Check if a file exists.
    pub async fn exists(&self, path: impl AsRef<Path>) -> bool {
        self.files.read().await.contains_key(path.as_ref())
    }

    /// List all files in the mock filesystem.
    pub async fn list_files(&self) -> Vec<PathBuf> {
        self.files.read().await.keys().cloned().collect()
    }
}

#[async_trait::async_trait]
impl AsyncFileSystem for MockFs {
    async fn read_file(&self, path: &Path) -> Result<Vec<u8>, ComputerError> {
        self.files.read().await.get(path).cloned().ok_or_else(|| {
            ComputerError::IOError(
                format!("File not found: {}", path.display()),
                Some(std::io::ErrorKind::NotFound),
            )
        })
    }

    async fn write_file(&self, path: &Path, data: &[u8]) -> Result<(), ComputerError> {
        self.files
            .write()
            .await
            .insert(path.to_path_buf(), data.to_vec());
        Ok(())
    }

    async fn delete_file(&self, path: &Path) -> Result<(), ComputerError> {
        self.files.write().await.remove(path);
        Ok(())
    }

    async fn file_exists(&self, path: &Path) -> Result<bool, ComputerError> {
        Ok(self.files.read().await.contains_key(path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_mock_fs_read_write() {
        let fs = MockFs::new();

        // File doesn't exist initially
        assert!(fs.read_file(Path::new("/test.txt")).await.is_err());
        assert!(!fs.file_exists(Path::new("/test.txt")).await.unwrap());

        // Write a file
        fs.write_file(Path::new("/test.txt"), b"hello world")
            .await
            .unwrap();

        // Read it back
        let content = fs.read_file(Path::new("/test.txt")).await.unwrap();
        assert_eq!(content, b"hello world");
        assert!(fs.file_exists(Path::new("/test.txt")).await.unwrap());
    }

    #[tokio::test]
    async fn test_mock_fs_delete() {
        let fs = MockFs::new();

        fs.write_file(Path::new("/test.txt"), b"hello")
            .await
            .unwrap();
        assert!(fs.exists(Path::new("/test.txt")).await);

        fs.delete_file(Path::new("/test.txt")).await.unwrap();
        assert!(!fs.exists(Path::new("/test.txt")).await);
    }

    #[tokio::test]
    async fn test_mock_fs_set_file() {
        let fs = MockFs::new();

        fs.set_file("/preset.txt", b"preset content").await;

        let content = fs.read_file(Path::new("/preset.txt")).await.unwrap();
        assert_eq!(content, b"preset content");
    }
}
