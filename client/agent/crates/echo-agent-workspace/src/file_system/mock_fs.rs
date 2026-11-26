use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use tokio::sync::RwLock;

use crate::file_system::{AsyncFileSystem, FsError};

pub struct MockFs {
    root: PathBuf,
    files: RwLock<HashMap<PathBuf, Vec<u8>>>,
}

#[async_trait::async_trait]
impl AsyncFileSystem for MockFs {
    fn root(&self) -> &Path {
        &self.root
    }

    async fn exists(&self, path: &Path) -> Result<bool, FsError> {
        let map = self.files.read().await;
        Ok(map.contains_key(path))
    }

    async fn read_file(&self, path: &Path) -> Result<Vec<u8>, FsError> {
        let map = self.files.read().await;
        if let Some(bytes) = map.get(path) {
            Ok(bytes.clone())
        } else {
            Err(io::Error::new(io::ErrorKind::NotFound, "File not found").into())
        }
    }

    async fn try_read_file(&self, path: &Path) -> Result<Option<Vec<u8>>, FsError> {
        let map = self.files.read().await;
        Ok(map.get(path).cloned())
    }

    async fn write_file(&self, path: &Path, data: &[u8]) -> Result<(), FsError> {
        let mut map = self.files.write().await;
        map.insert(path.to_path_buf(), data.to_vec());
        Ok(())
    }

    async fn delete_file(&self, path: &Path) -> Result<(), FsError> {
        let mut map = self.files.write().await;
        map.remove(path);
        Ok(())
    }
}

impl MockFs {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            files: RwLock::new(HashMap::new()),
        }
    }
}
