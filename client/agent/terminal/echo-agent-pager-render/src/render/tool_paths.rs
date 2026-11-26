//! Read/Edit tool-path resolution and surface formatting.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use unicode_width::UnicodeWidthStr;

use super::line_utils::truncate_str;

/// Read/Edit tool-header path paint surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolPathSurface {
    /// Basename only.
    Collapsed,
    /// Relative to session cwd when lexically contained; else normalized.
    Expanded,
    /// Normalized target spelling for the modal preamble.
    Fullscreen,
}

#[derive(Debug, Clone)]
struct ResolvedToolPath {
    display_path: PathBuf,
    relative_to_cwd: Option<String>,
}

fn expand_tilde_with_home(path: &Path, home: Option<&Path>) -> Option<PathBuf> {
    use std::path::Component;

    let mut components = path.components();
    let Some(Component::Normal(first)) = components.next() else {
        return Some(path.to_path_buf());
    };
    if first != "~" {
        return Some(path.to_path_buf());
    }

    let mut expanded = home?.to_path_buf();
    for component in components {
        match component {
            Component::Prefix(_) | Component::RootDir => {}
            _ => expanded.push(component.as_os_str()),
        }
    }
    Some(expanded)
}

/// Resolve the path the OS should receive, preserving `.`/`..` and symlink semantics.
pub(crate) fn resolve_tool_path_target_with_home(
    path: &Path,
    cwd: Option<&Path>,
    home: Option<&Path>,
) -> Option<PathBuf> {
    use std::path::Component;

    let target = expand_tilde_with_home(path, home)?;
    if target.is_absolute() || matches!(target.components().next(), Some(Component::Prefix(_))) {
        return Some(target);
    }
    Some(match cwd {
        Some(cwd) => cwd.join(target),
        None => target,
    })
}

fn non_empty_rel(rel: &Path) -> Option<String> {
    let value = rel.to_string_lossy();
    if value.is_empty() {
        None
    } else {
        Some(value.into_owned())
    }
}

fn home_dir() -> Option<&'static Path> {
    static HOME: OnceLock<Option<PathBuf>> = OnceLock::new();
    HOME.get_or_init(dirs::home_dir).as_deref()
}

/// Resolve the path-native target for OSC8 or background filesystem work.
pub fn resolve_tool_path_target(path: &str, cwd: Option<&Path>) -> Option<PathBuf> {
    resolve_tool_path_target_with_home(Path::new(path), cwd, home_dir())
}

fn resolve_tool_path_with_home(
    path: &str,
    cwd: Option<&Path>,
    home: Option<&Path>,
) -> ResolvedToolPath {
    let target = resolve_tool_path_target_with_home(Path::new(path), cwd, home);
    let display_path = target
        .as_deref()
        .map(echo_agent_paths::normalize_lexically)
        .unwrap_or_else(|| PathBuf::from(path));
    let relative_to_cwd = target.as_ref().and_then(|_| {
        let cwd = echo_agent_paths::normalize_lexically(cwd?);
        display_path.strip_prefix(cwd).ok().and_then(non_empty_rel)
    });
    ResolvedToolPath {
        display_path,
        relative_to_cwd,
    }
}

fn resolve_tool_path(path: &str, cwd: Option<&Path>) -> ResolvedToolPath {
    resolve_tool_path_with_home(path, cwd, home_dir())
}

fn path_for_fullscreen_header(path: &str, cwd: Option<&Path>) -> String {
    resolve_tool_path(path, cwd)
        .display_path
        .to_string_lossy()
        .into_owned()
}

fn path_for_expanded_header(path: &str, cwd: Option<&Path>) -> String {
    let resolved = resolve_tool_path(path, cwd);
    resolved
        .relative_to_cwd
        .unwrap_or_else(|| resolved.display_path.to_string_lossy().into_owned())
}

/// Shorten a file path to fit within `budget` display columns using fish-style
/// component shortening.
pub fn shorten_path(path: &str, budget: usize) -> String {
    if budget == 0 {
        return String::new();
    }
    if path.width() <= budget {
        return path.to_string();
    }

    let parts: Vec<&str> = path.split('/').collect();
    if parts.len() <= 1 {
        return truncate_str(path, budget);
    }

    let mut shortened: Vec<String> = parts.iter().map(|part| part.to_string()).collect();
    let last_idx = shortened.len() - 1;
    for i in 0..last_idx {
        if shortened.iter().map(String::len).sum::<usize>() + shortened.len() - 1 <= budget {
            break;
        }
        if let Some(first) = parts[i].chars().next() {
            shortened[i] = first.to_string();
        }
    }

    let joined = shortened.join("/");
    if joined.width() <= budget {
        return joined;
    }

    let mut tail_start = 0;
    for (i, _) in path.char_indices() {
        if i == 0 {
            continue;
        }
        if path.as_bytes().get(i.wrapping_sub(1)) == Some(&b'/') {
            let candidate = format!("\u{2026}{}", &path[i - 1..]);
            if candidate.width() <= budget {
                tail_start = i - 1;
                break;
            }
        }
    }
    if tail_start > 0 {
        let result = format!("\u{2026}{}", &path[tail_start..]);
        if result.width() <= budget {
            return result;
        }
    }
    truncate_str(path, budget)
}

pub fn path_basename(path: &str, budget: usize) -> String {
    let name = path
        .trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .next()
        .filter(|name| !name.is_empty())
        .unwrap_or(path);
    truncate_str(name, budget)
}

/// Compatibility formatter: compact basename with `Some(width)`, else stored path.
pub fn path_for_tool_header(path: &str, width: Option<usize>, reserved: usize) -> String {
    match width {
        Some(width) => path_basename(path, width.saturating_sub(reserved)),
        None => path.to_string(),
    }
}

/// Path text for a Read/Edit tool-header surface.
pub fn path_for_tool_surface(
    path: &str,
    surface: ToolPathSurface,
    cwd: Option<&Path>,
    width: Option<usize>,
    reserved: usize,
) -> String {
    match surface {
        ToolPathSurface::Collapsed => {
            let budget = width.unwrap_or(usize::MAX).saturating_sub(reserved);
            path_basename(path, budget)
        }
        ToolPathSurface::Expanded => path_for_expanded_header(path, cwd),
        ToolPathSurface::Fullscreen => path_for_fullscreen_header(path, cwd),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shorten_path_fits() {
        assert_eq!(shorten_path("src/main.rs", 20), "src/main.rs");
    }

    #[test]
    fn shorten_path_fish_style() {
        let result = shorten_path("terminal/echo-agent-pager/src/views/foo.rs", 25);
        assert!(result.width() <= 25, "got: {result}");
        assert!(result.ends_with("foo.rs"), "got: {result}");
    }

    #[test]
    fn shorten_path_front_truncate() {
        let result = shorten_path(
            "terminal/echo-agent-pager/src/views/very_long_filename.rs",
            20,
        );
        assert!(result.width() <= 20, "got: {result}");
    }

    #[test]
    fn shorten_path_no_separator() {
        assert_eq!(shorten_path("verylongfilename.rs", 10), "verylongf\u{2026}");
    }

    #[test]
    fn shorten_path_zero_budget() {
        assert_eq!(shorten_path("src/main.rs", 0), "");
    }

    #[test]
    fn path_basename_handles_native_and_mixed_separators() {
        assert_eq!(
            path_basename("/Users/me/project/src/main.rs", 80),
            "main.rs"
        );
        assert_eq!(path_basename("src/main.rs", 80), "main.rs");
        assert_eq!(
            path_basename(r"C:\Users\me/project/src/main.rs", 80),
            "main.rs"
        );
        assert_eq!(path_basename(r"C:\Users\me\project\src\", 80), "src");
        assert_eq!(path_basename("/Users/me/project/src/", 80), "src");
    }

    #[test]
    fn path_basename_truncates_to_budget() {
        assert_eq!(
            path_basename("/x/verylongfilename.rs", 10),
            "verylongf\u{2026}"
        );
        assert_eq!(path_basename("src/main.rs", 0), "");
    }

    #[test]
    fn collapsed_surface_is_basename() {
        assert_eq!(
            path_for_tool_surface(
                "/Users/me/project/src/main.rs",
                ToolPathSurface::Collapsed,
                None,
                Some(80),
                "Read ".len()
            ),
            "main.rs"
        );
    }

    #[test]
    fn expanded_surface_normalizes_and_classifies_against_cwd() {
        let cwd = Path::new("/Users/me/project");
        assert_eq!(
            path_for_tool_surface(
                "/Users/me/project/src/main.rs",
                ToolPathSurface::Expanded,
                Some(cwd),
                None,
                0
            ),
            "src/main.rs"
        );
        assert_eq!(
            path_for_tool_surface(
                "src/./nested/../main.rs",
                ToolPathSurface::Expanded,
                Some(cwd),
                None,
                0
            ),
            "src/main.rs"
        );
        assert_eq!(
            path_for_tool_surface(
                "../outside.rs",
                ToolPathSurface::Expanded,
                Some(cwd),
                None,
                0
            ),
            "/Users/me/outside.rs"
        );
    }

    #[test]
    fn filesystem_target_preserves_symlink_sensitive_parent_segments() {
        let raw = Path::new("/repo/link/../target.rs");
        assert_eq!(
            resolve_tool_path_target_with_home(raw, None, Some(Path::new("/home/me"))),
            Some(raw.to_path_buf())
        );
    }

    #[cfg(unix)]
    #[test]
    fn tilde_expansion_uses_native_components_and_fails_closed_without_home() {
        let home = Path::new("/home/me");
        let cwd = Path::new("/repo");
        assert_eq!(
            resolve_tool_path_target_with_home(Path::new("~//foo.rs"), Some(cwd), Some(home)),
            Some(home.join("foo.rs"))
        );
        assert_eq!(
            resolve_tool_path_target_with_home(Path::new("~/dir/../foo.rs"), Some(cwd), Some(home)),
            Some(home.join("dir/../foo.rs"))
        );
        assert_eq!(
            resolve_tool_path_target_with_home(Path::new("~/foo.rs"), Some(cwd), None),
            None
        );
        let unresolved = resolve_tool_path_with_home("~/foo.rs", Some(cwd), None);
        assert_eq!(unresolved.display_path, PathBuf::from("~/foo.rs"));
        assert_eq!(unresolved.relative_to_cwd, None);
    }

    #[test]
    fn expanded_outside_cwd_stays_normalized_target() {
        let cwd = Path::new("/Users/me/project");
        let got =
            path_for_tool_surface("/etc/hosts", ToolPathSurface::Expanded, Some(cwd), None, 0);
        assert!(Path::new(&got).is_absolute(), "got {got}");
        assert!(got.ends_with("hosts"), "got {got}");
        assert!(!got.starts_with("/Users/me/project"), "got {got}");
    }

    #[test]
    fn expanded_surface_uses_worktree_cwd() {
        let cwd = Path::new("/Users/me/.echo-agent/worktrees/foo");
        let path = "/Users/me/.echo-agent/worktrees/foo/crates/x/a.rs";
        assert_eq!(
            path_for_tool_surface(path, ToolPathSurface::Expanded, Some(cwd), None, 0),
            "crates/x/a.rs"
        );
    }

    #[test]
    fn fullscreen_surface_uses_anchored_or_honestly_relative_target() {
        let cwd = Path::new("/Users/me/project");
        assert_eq!(
            path_for_tool_surface(
                "src/main.rs",
                ToolPathSurface::Fullscreen,
                Some(cwd),
                None,
                0
            ),
            "/Users/me/project/src/main.rs"
        );
        let relative = resolve_tool_path("src/../main.rs", None);
        assert_eq!(relative.display_path, PathBuf::from("main.rs"));
        assert!(!relative.display_path.is_absolute());
    }

    #[test]
    fn home_relative_target_preserves_filesystem_spelling_for_io() {
        let Some(home) = dirs::home_dir() else {
            return;
        };
        assert_eq!(resolve_tool_path_target("~", None), Some(home.clone()));
        assert_eq!(
            resolve_tool_path_target("~/project/../notes.md", None),
            Some(home.join("project/../notes.md"))
        );
        assert_eq!(
            resolve_tool_path("~/project/../notes.md", None).display_path,
            echo_agent_paths::normalize_lexically(&home.join("notes.md"))
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_tilde_and_drive_relative_targets_keep_native_semantics() {
        let home = Path::new(r"C:\Users\me");
        let cwd = Path::new(r"C:\repo");
        assert_eq!(
            resolve_tool_path_target_with_home(Path::new(r"~\foo.rs"), Some(cwd), Some(home)),
            Some(home.join("foo.rs"))
        );
        assert_eq!(
            resolve_tool_path_target_with_home(Path::new(r"C:foo.rs"), Some(cwd), Some(home)),
            Some(PathBuf::from(r"C:foo.rs"))
        );
        let resolved = resolve_tool_path(r"C:foo.rs", Some(cwd));
        assert_eq!(resolved.display_path, PathBuf::from(r"C:foo.rs"));
        assert!(!resolved.display_path.is_absolute());
        assert_eq!(
            path_for_tool_surface(r"C:foo.rs", ToolPathSurface::Fullscreen, Some(cwd), None, 0),
            r"C:foo.rs"
        );
    }

    #[cfg(unix)]
    #[test]
    fn expanded_surface_does_not_dereference_symlink_aliases() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real_project");
        std::fs::create_dir_all(real.join("src")).unwrap();
        std::fs::write(real.join("src/main.rs"), b"fn main() {}").unwrap();
        let link = dir.path().join("link_project");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        let file_via_real = real.join("src/main.rs");
        assert_eq!(
            path_for_tool_surface(
                file_via_real.to_str().unwrap(),
                ToolPathSurface::Expanded,
                Some(link.as_path()),
                None,
                0
            ),
            file_via_real.to_string_lossy()
        );
    }
}
