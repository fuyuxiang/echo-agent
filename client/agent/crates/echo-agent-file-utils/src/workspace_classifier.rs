use std::path::{Path, PathBuf};

const EXCLUDED_DIR_NAMES: &[&str] = &[
    ".echo-agent",
    ".cache",
    ".daemon",
    ".config",
    ".npm",
    ".cargo",
    ".rustup",
    ".vscode",
    ".gemini",
    ".hermes",
    ".claude",
];

fn known_os_dirs() -> Vec<PathBuf> {
    [
        dirs::desktop_dir(),
        dirs::download_dir(),
        dirs::document_dir(),
        dirs::audio_dir(),
        dirs::video_dir(),
        dirs::picture_dir(),
        dirs::public_dir(),
    ]
    .into_iter()
    .flatten()
    .collect()
}

pub fn is_project_dir(cwd: &Path) -> bool {
    if cwd.as_os_str().is_empty() || cwd.parent().is_none() {
        return false;
    }

    if cwd.ancestors().any(|p| p.join(".git").exists()) {
        return true;
    }

    if has_excluded_component(cwd) {
        return false;
    }

    if is_platform_system_dir(cwd) {
        return false;
    }

    let Some(home) = dirs::home_dir() else {
        return false;
    };

    if cwd == home {
        return false;
    }

    if is_platform_home_excluded(cwd, &home) {
        return false;
    }

    if known_os_dirs().iter().any(|d| cwd == d) {
        return false;
    }

    true
}

#[cfg(not(target_os = "windows"))]
fn is_platform_system_dir(cwd: &Path) -> bool {
    if cwd == Path::new("/tmp")
        || cwd.starts_with("/tmp/")
        || cwd == Path::new("/var/tmp")
        || cwd.starts_with("/var/tmp/")
        || cwd.starts_with("/var/folders/")
    {
        return true;
    }

    #[cfg(target_os = "macos")]
    if cwd == Path::new("/private/tmp")
        || cwd.starts_with("/private/tmp/")
        || cwd == Path::new("/private/var/tmp")
        || cwd.starts_with("/private/var/tmp/")
        || cwd.starts_with("/private/var/folders/")
    {
        return true;
    }

    #[cfg(target_os = "linux")]
    if cwd == Path::new("/root") {
        return true;
    }

    false
}

#[cfg(target_os = "windows")]
fn is_platform_system_dir(cwd: &Path) -> bool {
    if let Ok(temp) = std::env::var("TEMP").or_else(|_| std::env::var("TMP")) {
        if cwd.starts_with(&temp) {
            return true;
        }
    }

    let path_lower = cwd.to_string_lossy().to_lowercase();
    if path_lower.contains("\\windows\\")
        || path_lower.ends_with("\\windows")
        || path_lower.contains("\\program files")
    {
        return true;
    }

    if cwd.parent().map_or(false, |p| p.parent().is_none()) && cwd.to_string_lossy().len() <= 3 {
        return true;
    }

    false
}

#[cfg(target_os = "macos")]
fn is_platform_home_excluded(cwd: &Path, home: &Path) -> bool {
    if cwd.starts_with(home.join("Library"))
        && !cwd.starts_with(home.join("Library/Mobile Documents"))
    {
        return true;
    }
    false
}

#[cfg(target_os = "linux")]
fn is_platform_home_excluded(cwd: &Path, home: &Path) -> bool {
    let Ok(relative) = cwd.strip_prefix(home) else {
        return false;
    };
    if relative.components().count() != 1 {
        return false;
    }
    let Some(std::path::Component::Normal(name)) = relative.components().next() else {
        return false;
    };
    let name = name.to_string_lossy().to_lowercase();
    [
        "desktop",
        "downloads",
        "documents",
        "pictures",
        "music",
        "videos",
    ]
    .contains(&name.as_str())
}

#[cfg(target_os = "windows")]
fn is_platform_home_excluded(_cwd: &Path, _home: &Path) -> bool {
    false
}

fn has_excluded_component(path: &Path) -> bool {
    for component in path.components() {
        if let std::path::Component::Normal(name) = component {
            let name_lower = name.to_string_lossy().to_lowercase();

            if EXCLUDED_DIR_NAMES.contains(&name_lower.as_str()) {
                return true;
            }

            if name_lower.starts_with(".echo-agent-") {
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(not(target_os = "windows"))]
    mod posix {
        use super::*;

        #[test]
        fn root_is_unsafe() {
            assert!(!is_project_dir(Path::new("/")));
        }

        #[test]
        fn tmp_is_unsafe() {
            assert!(!is_project_dir(Path::new("/tmp")));
            assert!(!is_project_dir(Path::new("/tmp/scratch")));
        }

        #[test]
        fn tmp_prefix_not_greedy() {
            assert!(is_project_dir(Path::new("/tmpdata/foo")));
        }

        #[test]
        fn var_folders_is_unsafe() {
            assert!(!is_project_dir(Path::new("/var/folders/ab/cd")));
        }

        #[test]
        fn deep_project_is_safe() {
            assert!(is_project_dir(Path::new("/Users/someone/my-project/src")));
        }

        #[test]
        fn home_subdir_is_safe() {
            assert!(is_project_dir(Path::new("/Users/someone/my-project")));
        }
    }

    #[cfg(target_os = "macos")]
    mod macos {
        use super::*;

        #[test]
        fn private_tmp_is_unsafe() {
            assert!(!is_project_dir(Path::new("/private/tmp")));
            assert!(!is_project_dir(Path::new("/private/tmp/scratch")));
            assert!(!is_project_dir(Path::new("/private/var/folders/ab/cd")));
        }

        #[test]
        fn library_is_unsafe() {
            if let Some(home) = dirs::home_dir() {
                assert!(!is_project_dir(&home.join("Library")));
                assert!(!is_project_dir(&home.join("Library/Caches")));
                assert!(!is_project_dir(&home.join("Library/Application Support")));
            }
        }

        #[test]
        fn icloud_drive_projects_are_safe() {
            if let Some(home) = dirs::home_dir() {
                assert!(is_project_dir(&home.join(
                    "Library/Mobile Documents/com~apple~CloudDocs/Projects/my-app"
                )));
            }
        }
    }

    #[cfg(target_os = "linux")]
    mod linux {
        use super::*;

        #[test]
        fn bare_root_is_unsafe() {
            assert!(!is_project_dir(Path::new("/root")));
        }

        #[test]
        fn root_project_is_safe() {
            assert!(is_project_dir(Path::new("/root/my-project")));
        }
    }

    mod config_and_cache {
        use super::*;

        #[test]
        fn echo_agent_dirs_are_unsafe() {
            if let Some(home) = dirs::home_dir() {
                assert!(!is_project_dir(&home.join(".echo-agent")));
                assert!(!is_project_dir(&home.join(".echo-agent/bin")));
            }
        }

        #[test]
        fn echo_agent_prefixed_dirs_are_unsafe() {
            if let Some(home) = dirs::home_dir() {
                assert!(!is_project_dir(&home.join(".echo-agent-proxy-work")));
            }
        }

        #[test]
        fn cache_dirs_are_unsafe() {
            if let Some(home) = dirs::home_dir() {
                assert!(!is_project_dir(&home.join(".cache/zoe-proc")));
                assert!(!is_project_dir(&home.join(".config/nvim")));
            }
        }

        #[test]
        fn other_ai_tool_dirs_are_unsafe() {
            if let Some(home) = dirs::home_dir() {
                assert!(!is_project_dir(&home.join(".gemini/antigravity")));
                assert!(!is_project_dir(&home.join(".hermes/kanban")));
                assert!(!is_project_dir(&home.join(".claude/projects")));
            }
        }
    }

    mod home_and_os_dirs {
        use super::*;

        #[test]
        fn home_is_unsafe() {
            if let Some(home) = dirs::home_dir() {
                assert!(!is_project_dir(&home));
            }
        }

        #[test]
        fn home_project_is_safe() {
            if let Some(home) = dirs::home_dir() {
                assert!(is_project_dir(&home.join("my-project")));
            }
        }

        #[test]
        fn bare_desktop_is_unsafe() {
            if let Some(d) = dirs::desktop_dir() {
                assert!(!is_project_dir(&d));
            }
        }

        #[test]
        fn desktop_project_is_safe() {
            if let Some(d) = dirs::desktop_dir() {
                assert!(is_project_dir(&d.join("my-project")));
            }
        }

        #[test]
        fn bare_downloads_is_unsafe() {
            if let Some(d) = dirs::download_dir() {
                assert!(!is_project_dir(&d));
            }
        }

        #[test]
        fn bare_documents_is_unsafe() {
            if let Some(d) = dirs::document_dir() {
                assert!(!is_project_dir(&d));
            }
        }
    }

    mod edge_cases {
        use super::*;

        #[test]
        fn empty_path_is_unsafe() {
            assert!(!is_project_dir(Path::new("")));
        }

        #[cfg(not(target_os = "windows"))]
        #[test]
        fn unicode_paths_work() {
            assert!(is_project_dir(Path::new(
                "/Users/me/code/\u{D3F4}\u{B9AC}\u{B9C8}\u{CF13}"
            )));
        }

        #[cfg(not(target_os = "windows"))]
        #[test]
        fn spaces_work() {
            assert!(is_project_dir(Path::new("/Users/me/My Projects/cool app")));
        }
    }

    mod git_detection {
        use super::*;

        #[test]
        fn inside_git_repo_is_safe() {
            let tmp = tempfile::tempdir().unwrap();
            std::fs::create_dir(tmp.path().join(".git")).unwrap();
            assert!(is_project_dir(tmp.path()));
        }

        #[test]
        fn subdirectory_of_git_repo_is_safe() {
            let tmp = tempfile::tempdir().unwrap();
            std::fs::create_dir(tmp.path().join(".git")).unwrap();
            let sub = tmp.path().join("deep/sub/dir");
            std::fs::create_dir_all(&sub).unwrap();
            assert!(is_project_dir(&sub));
        }
    }
}
