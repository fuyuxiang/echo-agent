//! File change events for live index updates.

use std::path::PathBuf;

/// Events that can trigger index updates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileEvent {
    /// A new file was created.
    Created {
        /// Path to the new file.
        path: PathBuf,
    },

    /// An existing file was modified.
    Modified {
        /// Path to the modified file.
        path: PathBuf,
    },

    /// A file was deleted.
    Deleted {
        /// Path to the deleted file.
        path: PathBuf,
    },

    /// A file was renamed/moved.
    Renamed {
        /// Original path.
        from: PathBuf,
        /// New path.
        to: PathBuf,
    },
}

impl FileEvent {
    /// Get the primary path associated with this event.
    pub fn path(&self) -> &PathBuf {
        match self {
            FileEvent::Created { path } => path,
            FileEvent::Modified { path } => path,
            FileEvent::Deleted { path } => path,
            FileEvent::Renamed { to, .. } => to,
        }
    }

    /// Check if this event requires reparsing the file content.
    pub fn requires_reparse(&self) -> bool {
        match self {
            FileEvent::Created { .. } => true,
            FileEvent::Modified { .. } => true,
            FileEvent::Deleted { .. } => false,
            FileEvent::Renamed { .. } => false, // Only path update needed
        }
    }

    /// Check if this event affects an existing indexed file.
    pub fn affects_existing(&self) -> bool {
        match self {
            FileEvent::Created { .. } => false,
            FileEvent::Modified { .. } => true,
            FileEvent::Deleted { .. } => true,
            FileEvent::Renamed { .. } => true,
        }
    }
}

impl std::fmt::Display for FileEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FileEvent::Created { path } => write!(f, "Created: {}", path.display()),
            FileEvent::Modified { path } => write!(f, "Modified: {}", path.display()),
            FileEvent::Deleted { path } => write!(f, "Deleted: {}", path.display()),
            FileEvent::Renamed { from, to } => {
                write!(f, "Renamed: {} -> {}", from.display(), to.display())
            }
        }
    }
}
