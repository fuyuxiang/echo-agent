//! Location type for query results.
//!
//! Location uses 1-indexed line and column numbers for LSP compatibility.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::Range;

/// A location in the codebase, used for query results.
///
/// Following LSP protocol conventions:
/// - `file_path`: Absolute path to the file
/// - `line`: 1-indexed line number
/// - `column`: 1-indexed column number
/// - `range`: Full range information (0-indexed internally)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "camelCase")]
pub struct Location {
    /// File path (absolute)
    pub file_path: PathBuf,
    /// Line number (1-indexed)
    pub line: usize,
    /// Column number (1-indexed)
    pub column: usize,
    /// Full range information (0-indexed internally)
    pub range: Range,
}

impl Location {
    /// Create a new location with 1-indexed line and column.
    pub fn new(file_path: PathBuf, line: usize, column: usize, range: Range) -> Self {
        Self {
            file_path,
            line,
            column,
            range,
        }
    }

    /// Create a location from a range (automatically converts to 1-indexed).
    pub fn from_range(file_path: PathBuf, range: Range) -> Self {
        Self {
            file_path,
            line: range.start_line_1indexed(),
            column: range.start_column_1indexed(),
            range,
        }
    }

    /// Get the file path.
    pub fn file_path(&self) -> &PathBuf {
        &self.file_path
    }

    /// Alias for file_path() - for compatibility.
    pub fn path(&self) -> &PathBuf {
        &self.file_path
    }

    /// Get the 1-indexed line number.
    pub fn line(&self) -> usize {
        self.line
    }

    /// Get the 1-indexed column number.
    pub fn column(&self) -> usize {
        self.column
    }

    /// Get the range (0-indexed internally).
    pub fn range(&self) -> &Range {
        &self.range
    }

    /// Get the file extension, if any.
    pub fn extension(&self) -> Option<&str> {
        self.file_path.extension().and_then(|e| e.to_str())
    }

    /// Get the parent directory of the file.
    pub fn parent_dir(&self) -> Option<&std::path::Path> {
        self.file_path.parent()
    }

    /// Get the 0-indexed line number (for internal use).
    pub fn line_0indexed(&self) -> usize {
        self.line.saturating_sub(1)
    }

    /// Get the 0-indexed column number (for internal use).
    pub fn column_0indexed(&self) -> usize {
        self.column.saturating_sub(1)
    }
}

impl std::fmt::Display for Location {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}:{}:{}",
            self.file_path.display(),
            self.line,
            self.column
        )
    }
}
