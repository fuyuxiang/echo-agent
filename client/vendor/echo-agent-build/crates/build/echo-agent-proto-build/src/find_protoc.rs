use anyhow::{Context, bail};
use std::env;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;

fn check_protoc_good(protoc: &Path) -> anyhow::Result<()> {
    let output = Command::new(protoc)
        .arg("--version")
        .output()
        .context("Failed to execute protoc")?;

    if !output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "protoc --version failed, likely dotslash is missing; \
             try `cargo install dotslash`; stdout: {stdout:?}, stderr: {stderr:?}"
        );
    }
    Ok(())
}

/// Resolve the executable itself, not the command name. Cargo interprets
/// rerun-if-changed paths relative to the build-script crate and a missing
/// `protoc` file there would make every later build dirty.
fn protoc_paths<'a>(path: &'a OsStr, cwd: &'a Path) -> impl Iterator<Item = PathBuf> + 'a {
    let executable = format!("protoc{}", env::consts::EXE_SUFFIX);
    env::split_paths(path).filter_map(move |directory| {
        let candidate = directory.join(&executable);
        let candidate = if candidate.is_absolute() {
            candidate
        } else {
            cwd.join(candidate)
        };
        // Also resolve Windows drive-relative PATH entries (for example C:bin)
        // without canonicalizing away a compiler symlink selected from PATH.
        let candidate = std::path::absolute(candidate).ok()?;
        let metadata = candidate.metadata().ok()?;
        if !metadata.is_file() {
            return None;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if metadata.permissions().mode() & 0o111 == 0 {
                return None;
            }
        }
        Some(candidate)
    })
}

fn is_github_actions() -> bool {
    env::var_os("GITHUB_ACTIONS").is_some()
}

/// Find `protoc` command.
///
/// Search order:
/// 1. `$PROTOC` environment variable (set by Bazel `build_script_env` or user override)
/// 2. `bin/protoc` walking up parent directories (dotslash wrapper for local dev)
/// 3. `protoc` on `$PATH` (system install or other tooling)
///
/// When `bin/protoc` exists but fails to execute (e.g. the dotslash wrapper running
/// in Bazel remote execution where `dotslash` is not installed), the error is not fatal —
/// we fall through to the PATH-based lookup instead.
///
/// Returns `Ok(None)` if not found and not in a strict environment (GitHub Actions).
pub fn find_protoc() -> anyhow::Result<Option<PathBuf>> {
    println!("cargo:rerun-if-env-changed=PROTOC");
    println!("cargo:rerun-if-env-changed=PATH");
    // 1. Check the PROTOC env var first. This is the standard override used by prost-build
    //    and is set by Bazel cargo_build_script build_script_env to point at a hermetic
    //    protoc binary instead of the dotslash wrapper.
    if let Ok(protoc_env) = env::var("PROTOC") {
        let protoc = PathBuf::from(&protoc_env);
        if protoc.try_exists()? {
            check_protoc_good(&protoc)?;
            return Ok(Some(protoc));
        }
    }

    // 2. Walk up directories looking for bin/protoc (dotslash wrapper).
    let cwd = env::current_dir()?;
    let mut dir = cwd.clone();
    let mut dir_rel = PathBuf::new();
    loop {
        // Return relative path to make build more deterministic.
        let protoc = dir_rel.join("bin/protoc");
        if protoc.try_exists()? {
            match check_protoc_good(&protoc) {
                Ok(()) => return Ok(Some(protoc)),
                Err(e) => {
                    // bin/protoc exists but can't execute — likely the dotslash wrapper
                    // in an environment without dotslash (e.g. Bazel remote execution).
                    // Fall through to PATH-based lookup below.
                    eprintln!(
                        "bin/protoc found at `{}` but failed to execute: {e:#}; \
                         trying protoc from PATH as fallback",
                        protoc.display()
                    );
                    break;
                }
            }
        }
        if !dir.pop() {
            break;
        }
        dir_rel.push("..");
    }

    // 3. Try protoc from PATH (system install or other tooling).
    if let Some(protoc) = env::var_os("PATH")
        .and_then(|path| protoc_paths(&path, &cwd).find(|protoc| check_protoc_good(protoc).is_ok()))
    {
        return Ok(Some(protoc));
    }

    // 4. Not found anywhere.
    if is_github_actions() {
        return Err(anyhow::anyhow!(
            "`protoc` not found (checked $PROTOC env, bin/protoc, and PATH)"
        ));
    }
    eprintln!("`protoc` not found; likely it is missing in docker image");
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn executable(directory: &Path) -> PathBuf {
        std::fs::create_dir_all(directory).unwrap();
        let path = directory.join(format!("protoc{}", env::consts::EXE_SUFFIX));
        std::fs::write(&path, b"locator fixture").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        path
    }

    #[test]
    fn path_lookup_returns_an_existing_absolute_executable_in_path_order() {
        let root = tempfile::tempdir().unwrap();
        let first = executable(&root.path().join("first compiler with spaces"));
        let second = executable(&root.path().join("second compiler"));
        let path = env::join_paths([first.parent().unwrap(), second.parent().unwrap()]).unwrap();
        let found = protoc_paths(&path, root.path()).next().unwrap();
        assert_eq!(found, first);
        assert!(found.is_absolute() && found.is_file());
        #[cfg(windows)]
        assert_eq!(found.file_name().unwrap(), "protoc.exe");
    }

    #[test]
    fn relative_path_entries_resolve_against_the_build_script_directory() {
        let root = tempfile::tempdir().unwrap();
        let expected = executable(&root.path().join("tools/bin"));
        let path = env::join_paths([Path::new("tools/bin")]).unwrap();
        assert_eq!(protoc_paths(&path, root.path()).next(), Some(expected));
    }

    #[test]
    fn path_lookup_skips_missing_files_and_directories() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("directory");
        std::fs::create_dir_all(directory.join(format!("protoc{}", env::consts::EXE_SUFFIX)))
            .unwrap();
        let expected = executable(&root.path().join("valid"));
        let path = env::join_paths([
            root.path().join("missing"),
            directory,
            expected.parent().unwrap().to_path_buf(),
        ])
        .unwrap();
        assert_eq!(protoc_paths(&path, root.path()).next(), Some(expected));
    }

    #[cfg(unix)]
    #[test]
    fn path_lookup_skips_non_executable_files() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let blocked = executable(&root.path().join("non executable"));
        std::fs::set_permissions(&blocked, std::fs::Permissions::from_mode(0o644)).unwrap();
        let expected = executable(&root.path().join("valid"));
        let path =
            env::join_paths([blocked.parent().unwrap(), expected.parent().unwrap()]).unwrap();
        assert_eq!(protoc_paths(&path, root.path()).next(), Some(expected));
    }
}
