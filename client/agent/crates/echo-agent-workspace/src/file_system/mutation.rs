use std::io;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use tokio::sync::watch;

struct ActiveLease {
    id: u64,
    path: PathBuf,
}

struct MutationRegistry {
    active: Vec<ActiveLease>,
    next_id: u64,
    changed: watch::Sender<()>,
}

static MUTATIONS: LazyLock<Mutex<MutationRegistry>> = LazyLock::new(|| {
    Mutex::new(MutationRegistry {
        active: Vec::new(),
        next_id: 0,
        changed: watch::channel(()).0,
    })
});

/// A lease covers the named file or subtree, including paths that did not yet exist.
pub struct WorkspaceMutationLease(u64);

impl Drop for WorkspaceMutationLease {
    fn drop(&mut self) {
        let mut registry = MUTATIONS.lock().unwrap();
        registry.active.retain(|entry| entry.id != self.0);
        registry.changed.send_replace(());
    }
}

fn canonical_mutation_path(path: &Path) -> io::Result<PathBuf> {
    match path.canonicalize() {
        Ok(path) => Ok(path),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let parent = path.parent().ok_or(error)?;
            let name = path
                .file_name()
                .ok_or_else(|| io::Error::other("invalid mutation path"))?;
            Ok(canonical_mutation_path(parent)?.join(name))
        }
        Err(error) => Err(error),
    }
}

fn try_claim(registry: &mut MutationRegistry, path: &Path) -> Option<WorkspaceMutationLease> {
    if registry
        .active
        .iter()
        .any(|entry| entry.path.starts_with(path) || path.starts_with(&entry.path))
    {
        return None;
    }
    registry.next_id += 1;
    let id = registry.next_id;
    registry.active.push(ActiveLease {
        id,
        path: path.to_owned(),
    });
    Some(WorkspaceMutationLease(id))
}

pub fn try_workspace_mutation(path: &Path) -> io::Result<WorkspaceMutationLease> {
    let path = canonical_mutation_path(path)?;
    try_claim(&mut MUTATIONS.lock().unwrap(), &path)
        .ok_or_else(|| io::Error::new(io::ErrorKind::WouldBlock, "workspace path is being changed"))
}

pub async fn acquire_workspace_mutation(path: &Path) -> io::Result<WorkspaceMutationLease> {
    let path = canonical_mutation_path(path)?;
    let mut changed = MUTATIONS.lock().unwrap().changed.subscribe();
    loop {
        if let Some(lease) = try_claim(&mut MUTATIONS.lock().unwrap(), &path) {
            return Ok(lease);
        }
        changed.changed().await.map_err(io::Error::other)?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn parent_operations_wait_without_blocking_an_independent_workspace() {
        let workspace = tempfile::tempdir().unwrap();
        let root = workspace.path();
        let left = root.join("left");
        let right = root.join("right");
        let child = left.join("file");
        let first = try_workspace_mutation(&child).unwrap();
        let parent = acquire_workspace_mutation(&left);
        tokio::pin!(parent);
        assert!(futures::poll!(&mut parent).is_pending());
        assert!(try_workspace_mutation(&right).is_ok());
        assert!(try_workspace_mutation(&left).is_err());
        drop(first);
        let parent = parent.await.unwrap();
        assert!(try_workspace_mutation(&child).is_err());
        drop(parent);
        assert!(try_workspace_mutation(&child).is_ok());
    }
}
