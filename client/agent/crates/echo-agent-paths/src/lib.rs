//! Type-safe path wrappers for absolute and relative UTF-8 paths.

use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// Error returned when creating an [`AbsPathBuf`] from an invalid path.
#[derive(Debug, thiserror::Error)]
pub enum AbsPathError {
    #[error("Path is not absolute: {input}")]
    NotAbsolute { input: String },
    #[error("Path is not valid UTF-8: {0}")]
    NotUtf8(std::path::PathBuf),
}

/// Error returned when creating a [`RelPathBuf`] from an invalid path.
#[derive(Debug, thiserror::Error)]
pub enum RelPathError {
    #[error("Path is not relative: {input}")]
    NotRelative { input: String },
    #[error("Path is not valid UTF-8: {0}")]
    NotUtf8(std::path::PathBuf),
}

/// Convert a path to absolute given a root directory.
///
/// For absolute paths, the `root` is ignored. For relative paths, joins with `root`.
pub trait ToAbsPath {
    fn to_abs_path(&self, root: &Path) -> Cow<'_, Path>;
}

/// Convert an absolute path to relative by stripping the root prefix.
///
/// Returns the path unchanged if not under `root`. For strict validation,
/// use [`RelPathBuf::from_absolute`] instead.
pub fn to_relative_path(root: &Path, abs_path: &Path) -> PathBuf {
    abs_path
        .strip_prefix(root)
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|_| abs_path.to_path_buf())
}

/// Convert a relative path to absolute by joining with root.
pub fn from_relative_path(root: &Path, rel_path: &Path) -> PathBuf {
    if rel_path.is_absolute() {
        rel_path.to_path_buf()
    } else {
        root.join(rel_path)
    }
}

/// Resolve `.` and `..` components without touching the filesystem.
///
/// Use only for lexical display or containment. If `b` is a symlink,
/// normalizing `a/b/../c` can name a different filesystem target than the OS
/// would resolve from the original spelling. Filesystem consumers must preserve
/// the original path or deliberately canonicalize it before use.
pub fn normalize_lexically(path: &Path) -> PathBuf {
    use std::path::Component;

    let mut components = Vec::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => match components.last() {
                Some(Component::Normal(_)) => {
                    components.pop();
                }
                Some(Component::RootDir) => {}
                _ => components.push(component),
            },
            _ => components.push(component),
        }
    }
    if components.is_empty() {
        PathBuf::from(".")
    } else {
        components.into_iter().collect()
    }
}

/// An absolute UTF-8 path.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
#[repr(transparent)]
pub struct AbsPathBuf(camino::Utf8PathBuf);

impl AbsPathBuf {
    /// Create from a PathBuf. Errors if not absolute or not UTF-8.
    pub fn new(path: std::path::PathBuf) -> Result<Self, AbsPathError> {
        if !path.is_absolute() {
            return Err(AbsPathError::NotAbsolute {
                input: path.display().to_string(),
            });
        }
        match camino::Utf8PathBuf::from_path_buf(path) {
            Ok(utf8) => Ok(Self(utf8)),
            Err(p) => Err(AbsPathError::NotUtf8(p)),
        }
    }

    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }

    pub fn as_path(&self) -> &std::path::Path {
        self.0.as_std_path()
    }

    pub fn to_path_buf(&self) -> std::path::PathBuf {
        self.as_path().to_path_buf()
    }

    pub fn into_string(self) -> String {
        self.0.into_string()
    }

    pub fn join(&self, path: impl AsRef<str>) -> Self {
        Self(self.0.join(path.as_ref()))
    }

    pub fn is_dir(&self) -> bool {
        self.0.is_dir()
    }

    /// Check if `self` contains `path` (normalizing `.`/`..`).
    pub fn contains_path(&self, path: &AbsPathBuf) -> bool {
        normalize_lexically(path.as_path()).starts_with(self.as_path())
    }
}

impl std::fmt::Display for AbsPathBuf {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl AsRef<str> for AbsPathBuf {
    fn as_ref(&self) -> &str {
        self.0.as_str()
    }
}

impl AsRef<std::path::Path> for AbsPathBuf {
    fn as_ref(&self) -> &std::path::Path {
        self.0.as_std_path()
    }
}

impl AsRef<OsStr> for AbsPathBuf {
    fn as_ref(&self) -> &OsStr {
        self.0.as_os_str()
    }
}

impl ToAbsPath for AbsPathBuf {
    fn to_abs_path(&self, _root: &Path) -> Cow<'_, Path> {
        Cow::Borrowed(self.as_path())
    }
}

impl ToAbsPath for &AbsPathBuf {
    fn to_abs_path(&self, _root: &Path) -> Cow<'_, Path> {
        Cow::Borrowed(self.as_path())
    }
}

impl ToAbsPath for &Path {
    fn to_abs_path(&self, root: &Path) -> Cow<'_, Path> {
        if self.is_absolute() {
            Cow::Borrowed(self)
        } else {
            Cow::Owned(root.join(self))
        }
    }
}

impl ToAbsPath for &PathBuf {
    fn to_abs_path(&self, root: &Path) -> Cow<'_, Path> {
        if self.is_absolute() {
            Cow::Borrowed(self.as_path())
        } else {
            Cow::Owned(root.join(self))
        }
    }
}

impl ToAbsPath for &str {
    fn to_abs_path(&self, root: &Path) -> Cow<'_, Path> {
        let path = Path::new(self);
        if path.is_absolute() {
            Cow::Owned(path.to_path_buf())
        } else {
            Cow::Owned(root.join(path))
        }
    }
}

impl ToAbsPath for String {
    fn to_abs_path(&self, root: &Path) -> Cow<'_, Path> {
        let path = Path::new(self);
        if path.is_absolute() {
            Cow::Owned(path.to_path_buf())
        } else {
            Cow::Owned(root.join(path))
        }
    }
}

/// A relative UTF-8 path. Serializes to/from string via serde.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
#[repr(transparent)]
pub struct RelPathBuf(camino::Utf8PathBuf);

impl RelPathBuf {
    /// Create from a string or PathBuf. Errors if absolute or not UTF-8.
    pub fn new(path: impl Into<PathBuf>) -> Result<Self, RelPathError> {
        let path = path.into();
        if path.is_absolute() {
            return Err(RelPathError::NotRelative {
                input: path.display().to_string(),
            });
        }
        match camino::Utf8PathBuf::from_path_buf(path) {
            Ok(utf8) => Ok(Self(utf8)),
            Err(p) => Err(RelPathError::NotUtf8(p)),
        }
    }

    /// Create by stripping root prefix. Errors if path not under root.
    pub fn from_absolute(root: &Path, abs_path: &Path) -> Result<Self, RelPathError> {
        let relative = abs_path
            .strip_prefix(root)
            .map_err(|_| RelPathError::NotRelative {
                input: abs_path.display().to_string(),
            })?;

        Self::new(relative)
    }

    /// Join with root to get absolute path.
    pub fn to_absolute(&self, root: &Path) -> PathBuf {
        root.join(&self.0)
    }

    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }

    pub fn as_path(&self) -> &std::path::Path {
        self.0.as_std_path()
    }

    pub fn to_path_buf(&self) -> PathBuf {
        self.as_path().to_path_buf()
    }

    pub fn into_string(self) -> String {
        self.0.into_string()
    }
}

impl std::fmt::Display for RelPathBuf {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl AsRef<str> for RelPathBuf {
    fn as_ref(&self) -> &str {
        self.0.as_str()
    }
}

impl AsRef<Path> for RelPathBuf {
    fn as_ref(&self) -> &Path {
        self.0.as_std_path()
    }
}

impl AsRef<OsStr> for RelPathBuf {
    fn as_ref(&self) -> &OsStr {
        self.0.as_os_str()
    }
}

impl ToAbsPath for RelPathBuf {
    fn to_abs_path(&self, root: &Path) -> Cow<'_, Path> {
        Cow::Owned(self.to_absolute(root))
    }
}

impl ToAbsPath for &RelPathBuf {
    fn to_abs_path(&self, root: &Path) -> Cow<'_, Path> {
        Cow::Owned(self.to_absolute(root))
    }
}

impl TryFrom<&str> for RelPathBuf {
    type Error = RelPathError;

    fn try_from(s: &str) -> Result<Self, Self::Error> {
        Self::new(PathBuf::from(s))
    }
}

impl TryFrom<String> for RelPathBuf {
    type Error = RelPathError;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        Self::new(PathBuf::from(s))
    }
}

impl TryFrom<camino::Utf8PathBuf> for RelPathBuf {
    type Error = RelPathError;

    fn try_from(path: camino::Utf8PathBuf) -> Result<Self, Self::Error> {
        if path.is_absolute() {
            return Err(RelPathError::NotRelative {
                input: path.into_string(),
            });
        }
        Ok(Self(path))
    }
}

impl From<RelPathBuf> for String {
    fn from(path: RelPathBuf) -> Self {
        path.0.into_string()
    }
}

impl From<RelPathBuf> for camino::Utf8PathBuf {
    fn from(path: RelPathBuf) -> Self {
        path.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn test_abs_path_buf_new() {
        let abs = AbsPathBuf::new(PathBuf::from("/home/user")).unwrap();
        assert_eq!(abs.as_str(), "/home/user");
    }

    #[test]
    fn test_abs_path_buf_new_relative_fails() {
        let result = AbsPathBuf::new(PathBuf::from("relative/path"));
        assert!(result.is_err());
    }

    #[cfg(unix)]
    #[test]
    fn test_abs_path_buf_contains_path() {
        let cwd = AbsPathBuf::new(PathBuf::from("/a/b")).unwrap();

        assert!(cwd.contains_path(&AbsPathBuf::new("/a/b/c".into()).unwrap()));
        assert!(cwd.contains_path(&AbsPathBuf::new("/a/b/c/d".into()).unwrap()));

        // exactly cwd
        assert!(cwd.contains_path(&AbsPathBuf::new("/a/b".into()).unwrap()));

        // path that escapes via parent
        assert!(!cwd.contains_path(&AbsPathBuf::new("/a/b/..".into()).unwrap()));
        assert!(!cwd.contains_path(&AbsPathBuf::new("/a/b/../c".into()).unwrap()));

        // going above root and back should normalize to under cwd
        assert!(cwd.contains_path(&AbsPathBuf::new("/a/b/../../../a/b".into()).unwrap()));
        // excessive above root still normalizes to root level
        assert!(!cwd.contains_path(&AbsPathBuf::new("/a/b/../../../../../c".into()).unwrap()));
    }

    #[cfg(unix)]
    #[test]
    fn contains_path_does_not_normalize_the_stored_root() {
        let non_normalized_root = AbsPathBuf::new(PathBuf::from("/a/b/..")).unwrap();
        let candidate = AbsPathBuf::new(PathBuf::from("/a/c")).unwrap();

        assert!(!non_normalized_root.contains_path(&candidate));
    }

    #[cfg(unix)]
    #[test]
    fn lexical_normalize_resolves_dot_segments_without_filesystem_access() {
        assert_eq!(
            normalize_lexically(Path::new("/work/project/src/./nested/../main.rs")),
            PathBuf::from("/work/project/src/main.rs")
        );
        assert_eq!(
            normalize_lexically(Path::new("../outside/./file.rs")),
            PathBuf::from("../outside/file.rs")
        );
        assert_eq!(normalize_lexically(Path::new("src/..")), PathBuf::from("."));
        assert_eq!(
            normalize_lexically(Path::new("/../../tmp")),
            PathBuf::from("/tmp")
        );
    }

    #[cfg(windows)]
    #[test]
    fn lexical_normalize_preserves_windows_prefixes() {
        assert_eq!(
            normalize_lexically(Path::new(r"C:\work\project\..\file.rs")),
            PathBuf::from(r"C:\work\file.rs")
        );
        assert_eq!(
            normalize_lexically(Path::new(r"C:\..\file.rs")),
            PathBuf::from(r"C:\file.rs")
        );
        assert_eq!(
            normalize_lexically(Path::new(r"C:..\outside.rs")),
            PathBuf::from(r"C:..\outside.rs")
        );
    }

    #[test]
    fn test_rel_path_buf_new() {
        let rel = RelPathBuf::new("src/main.rs").unwrap();
        assert_eq!(rel.as_str(), "src/main.rs");
    }

    #[test]
    fn test_rel_path_buf_new_absolute_fails() {
        let result = RelPathBuf::new("/absolute/path");
        assert!(result.is_err());
    }

    #[cfg(unix)]
    #[test]
    fn test_rel_path_buf_from_absolute() {
        let root = Path::new("/home/user/project");
        let abs = Path::new("/home/user/project/src/main.rs");
        let rel = RelPathBuf::from_absolute(root, abs).unwrap();
        assert_eq!(rel.as_str(), "src/main.rs");
    }

    #[cfg(unix)]
    #[test]
    fn test_rel_path_buf_from_absolute_not_under_root() {
        let root = Path::new("/home/user/project");
        let abs = Path::new("/other/path/file.rs");
        let result = RelPathBuf::from_absolute(root, abs);
        assert!(result.is_err());
    }

    #[cfg(unix)]
    #[test]
    fn test_rel_path_buf_to_absolute() {
        let rel = RelPathBuf::new("src/main.rs").unwrap();
        let root = Path::new("/home/user/project");
        assert_eq!(
            rel.to_absolute(root),
            PathBuf::from("/home/user/project/src/main.rs")
        );
    }

    #[test]
    fn test_rel_path_buf_conversions() {
        let rel = RelPathBuf::new("src/main.rs").unwrap();

        // Into String
        let rel_str: String = rel.clone().into();
        assert_eq!(rel_str, "src/main.rs");

        // TryFrom String
        let rel_from_str: RelPathBuf = "src/main.rs".try_into().unwrap();
        assert_eq!(rel_from_str, rel);
    }

    // Tests for helper functions

    #[cfg(unix)]
    #[test]
    fn test_to_relative_path_under_root() {
        let root = Path::new("/home/user/project");
        let abs = Path::new("/home/user/project/src/main.rs");
        assert_eq!(to_relative_path(root, abs), PathBuf::from("src/main.rs"));
    }

    #[cfg(unix)]
    #[test]
    fn test_to_relative_path_not_under_root() {
        let root = Path::new("/home/user/project");
        let abs = Path::new("/other/path/file.rs");
        assert_eq!(
            to_relative_path(root, abs),
            PathBuf::from("/other/path/file.rs")
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_to_relative_path_exact_root() {
        let root = Path::new("/home/user/project");
        let abs = Path::new("/home/user/project");
        assert_eq!(to_relative_path(root, abs), PathBuf::from(""));
    }

    #[cfg(unix)]
    #[test]
    fn test_from_relative_path_relative() {
        let root = Path::new("/home/user/project");
        let rel = Path::new("src/main.rs");
        assert_eq!(
            from_relative_path(root, rel),
            PathBuf::from("/home/user/project/src/main.rs")
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_from_relative_path_already_absolute() {
        let root = Path::new("/home/user/project");
        let abs = Path::new("/other/path/file.rs");
        assert_eq!(
            from_relative_path(root, abs),
            PathBuf::from("/other/path/file.rs")
        );
    }

    // Tests for serde serialization

    #[test]
    fn test_rel_path_buf_serde_roundtrip() {
        let rel = RelPathBuf::new("src/main.rs").unwrap();
        let json = serde_json::to_string(&rel).unwrap();
        assert_eq!(json, "\"src/main.rs\"");

        let deserialized: RelPathBuf = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized, rel);
    }

    #[test]
    fn test_rel_path_buf_serde_absolute_fails() {
        let json = "\"/absolute/path\"";
        let result: Result<RelPathBuf, _> = serde_json::from_str(json);
        assert!(result.is_err());
    }

    // Tests for ToAbsPath on &str and String

    #[cfg(unix)]
    #[test]
    fn test_to_abs_path_str_relative() {
        let root = Path::new("/home/user");
        let path: &str = "src/main.rs";
        let abs = path.to_abs_path(root);
        assert_eq!(abs.as_ref(), Path::new("/home/user/src/main.rs"));
    }

    #[cfg(unix)]
    #[test]
    fn test_to_abs_path_str_absolute() {
        let root = Path::new("/home/user");
        let path: &str = "/other/path";
        let abs = path.to_abs_path(root);
        assert_eq!(abs.as_ref(), Path::new("/other/path"));
    }

    #[cfg(unix)]
    #[test]
    fn test_to_abs_path_string_relative() {
        let root = Path::new("/home/user");
        let path: String = "src/main.rs".to_string();
        let abs = path.to_abs_path(root);
        assert_eq!(abs.as_ref(), Path::new("/home/user/src/main.rs"));
    }
}
