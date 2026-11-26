//! Filesystem locations for echoagent config files and binaries.

use std::path::PathBuf;

pub use echo_agent_home::{default_echo_agent_home, echo_agent_home, user_echo_agent_home};

/// Acceptance profiles isolate compatibility discovery as well as native data.
/// Never change OS HOME: normal launches retain their existing user discovery.
pub fn controlled_discovery_home() -> Option<PathBuf> {
    let isolated = std::env::var("ECHO_AGENT_REVIEW_ISOLATED").as_deref() == Ok("1")
        || std::env::var("ECHO_DESKTOP_REVIEW").as_deref() == Ok("1")
        || std::env::var("ECHO_DESKTOP_VALIDATION").as_deref() == Ok("1");
    if !isolated { return None; }
    // Fail closed rather than discovering real HOME when a validation launch
    // is malformed. Native entrypoints separately validate the private root.
    Some(review_discovery_root(std::env::var_os("ECHO_AGENT_HOME").map(PathBuf::from)))
}

fn review_discovery_root(root: Option<PathBuf>) -> PathBuf {
    root.filter(|root| root.is_absolute())
        .unwrap_or_else(|| std::env::temp_dir().join("echo-invalid-review-profile"))
        .join("compat-home")
}

pub fn resolve_discovery_home_with(
    controlled_home: Option<PathBuf>,
    normal_home: impl FnOnce() -> Option<PathBuf>,
) -> Option<PathBuf> {
    // Keep the fallback lazy: review launches never even invoke an OS HOME
    // resolver, much less inspect its .agents/.claude compatibility trees.
    controlled_home.or_else(normal_home)
}

pub fn discovery_home_dir() -> Option<PathBuf> {
    resolve_discovery_home_with(controlled_discovery_home(), echo_agent_home::os_home_dir)
}

#[cfg(target_os = "macos")]
const CLAUDE_MANAGED_SETTINGS_PATH: &str =
    "/Library/Application Support/ClaudeCode/managed-settings.json";
#[cfg(target_os = "linux")]
const CLAUDE_MANAGED_SETTINGS_PATH: &str = "/etc/claude-code/managed-settings.json";

/// Canonical echoagent application path: `$ECHO_AGENT_HOME/bin/echoagent` (Unix) or `echoagent.exe` (Windows).
pub fn echo_agent_application() -> PathBuf {
    echo_agent_application_in(&echo_agent_home())
}

/// [`echo_agent_application`] under an explicit home instead of `$ECHO_AGENT_HOME`.
pub fn echo_agent_application_in(home: &std::path::Path) -> PathBuf {
    let name = if cfg!(windows) {
        "echoagent.exe"
    } else {
        "echoagent"
    };
    home.join("bin").join(name)
}

/// System-wide config directory: `/etc/echoagent/` on Unix, `None` on Windows.
pub fn system_config_dir() -> Option<PathBuf> {
    if cfg!(unix) {
        Some(PathBuf::from("/etc/echoagent"))
    } else {
        None
    }
}

/// System path for the managed-settings.json used for settings compat, if it exists.
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub fn claude_managed_settings_path() -> Option<PathBuf> {
    let path = claude_managed_settings_probe_path()?;
    path.exists().then_some(path)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub fn claude_managed_settings_path() -> Option<PathBuf> {
    None
}

/// The platform path where managed-settings.json would live for settings
/// compat, whether or not it exists. `None` on unsupported platforms.
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub fn claude_managed_settings_probe_path() -> Option<PathBuf> {
    Some(controlled_discovery_home()
        .map(|home| home.join("system/claude/managed-settings.json"))
        .unwrap_or_else(|| PathBuf::from(CLAUDE_MANAGED_SETTINGS_PATH)))
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub fn claude_managed_settings_probe_path() -> Option<PathBuf> {
    None
}

/// Max bytes for a single directory name component (macOS APFS, Linux ext4,
/// NTFS all enforce 255 bytes).
const MAX_DIRNAME_BYTES: usize = 255;

/// Encode a CWD string into a filesystem-safe directory name component.
///
/// Short CWDs (URL-encoded form <= 255 bytes) use URL-encoding for backward
/// compatibility and human readability on disk.
///
/// Long CWDs (> 255 bytes encoded) use a compact `{slug}-{blake3_hex16}`
/// form that is always <= 57 bytes. Callers must write a `.cwd` metadata
/// file via [`ensure_sessions_cwd_dir`] so the original CWD can be
/// recovered by [`decode_cwd_from_dirname`].
pub fn encode_cwd_dirname(cwd: &str) -> String {
    let url_encoded = urlencoding::encode(cwd);
    if url_encoded.len() <= MAX_DIRNAME_BYTES {
        return url_encoded.into_owned();
    }
    let hash = blake3::hash(cwd.as_bytes());
    let hash16 = &hash.to_hex()[..16];
    let leaf = std::path::Path::new(cwd)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("workspace");
    let slug = slugify(leaf, 40);
    let slug = if slug.is_empty() { "workspace" } else { &slug };
    format!("{slug}-{hash16}")
}

/// Recover the original CWD from a sessions CWD directory.
///
/// Tries URL-decoding the directory name first (works for short/legacy dirs).
/// Falls back to reading a `.cwd` metadata file inside the directory (written
/// by [`ensure_sessions_cwd_dir`] for hash-based dirs).
pub fn decode_cwd_from_dirname(dir: &std::path::Path) -> Option<String> {
    let name = dir.file_name()?.to_str()?;
    if let Ok(decoded) = urlencoding::decode(name) {
        let s = decoded.into_owned();
        // URL-decoded absolute CWDs always start with `/` (Unix) or a drive
        // letter (Windows).  The slug-hash form never does, so this
        // distinguishes the two encodings unambiguously.
        if s.starts_with('/') || (cfg!(windows) && s.chars().nth(1) == Some(':')) {
            return Some(s);
        }
    }
    std::fs::read_to_string(dir.join(".cwd"))
        .ok()
        .map(|s| s.trim().to_string())
}

/// Best-effort chmod 0700 on Unix, no-op elsewhere: session dirs hold chat
/// history, and creators re-run on every touch so the mode self-heals.
/// Failures are logged (not returned): on chmod-hostile filesystems (FAT,
/// some network mounts) healing pre-existing loose dirs can never succeed,
/// and that must be visible.
pub fn set_dir_owner_only(dir: &std::path::Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Err(e) = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)) {
            tracing::debug!(?e, dir = %dir.display(), "failed to chmod session dir owner-only");
        }
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
    }
}

/// `create_dir_all` with directories born 0700 on Unix (no umask window),
/// plus a self-heal chmod for a pre-existing `dir`. Prefer this over bare
/// `create_dir_all` for anything under `sessions/`.
pub fn create_dir_all_owner_only(dir: &std::path::Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)?;
    }
    #[cfg(not(unix))]
    std::fs::create_dir_all(dir)?;
    set_dir_owner_only(dir);
    Ok(())
}

/// Build the CWD-level session directory path:
/// `echo_agent_home()/sessions/{encode_cwd_dirname(cwd)}`.
///
/// Does **not** create the directory on disk — use [`ensure_sessions_cwd_dir`]
/// when the directory must exist.
pub fn sessions_cwd_dir(cwd: &str) -> PathBuf {
    sessions_cwd_dir_in(&echo_agent_home(), cwd)
}

/// [`sessions_cwd_dir`] with an injectable echoagent home — the single source of
/// truth for the `sessions/<encoded-cwd>` path shape.
pub fn sessions_cwd_dir_in(echo_agent_home: &std::path::Path, cwd: &str) -> PathBuf {
    echo_agent_home
        .join("sessions")
        .join(encode_cwd_dirname(cwd))
}

/// Create the CWD-level session directory and write a `.cwd` metadata file
/// when hash-based encoding is used (long paths).
///
/// For short paths the `.cwd` file is not written because the directory name
/// itself is reversible via URL-decoding.
pub fn ensure_sessions_cwd_dir(cwd: &str) -> std::io::Result<PathBuf> {
    ensure_sessions_cwd_dir_in(&echo_agent_home(), cwd)
}

/// [`ensure_sessions_cwd_dir`] with an injectable echoagent home.
pub fn ensure_sessions_cwd_dir_in(
    echo_agent_home: &std::path::Path,
    cwd: &str,
) -> std::io::Result<PathBuf> {
    let encoded_name = encode_cwd_dirname(cwd);
    let dir = sessions_cwd_dir_in(echo_agent_home, cwd);
    // 0700 dir + root shield everything beneath (children with looser modes,
    // cwd-path dirnames, the session search index).
    create_dir_all_owner_only(&dir)?;
    set_dir_owner_only(&echo_agent_home.join("sessions"));
    // Hash-based encoding is in use when the dirname differs from the
    // plain URL-encoded form.  Write a `.cwd` file so decode can recover
    // the original path.  O_CREAT|O_EXCL via create_new avoids TOCTOU
    // races with parallel session starts.
    if encoded_name != urlencoding::encode(cwd).as_ref() {
        let cwd_file = dir.join(".cwd");
        match std::fs::File::create_new(&cwd_file) {
            Ok(mut f) => {
                std::io::Write::write_all(&mut f, cwd.as_bytes())?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
    }
    Ok(dir)
}

/// Generate a URL-safe slug from a string.
///
/// Lowercases, replaces non-alphanumeric chars with `-`, collapses
/// consecutive dashes, and truncates to `max_len` characters.
fn slugify(input: &str, max_len: usize) -> String {
    let mut result = String::with_capacity(input.len());
    let mut prev_dash = false;
    for c in input.to_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            result.push(c);
            prev_dash = false;
        } else if !prev_dash {
            result.push('-');
            prev_dash = true;
        }
    }
    let trimmed = result.trim_matches('-');
    trimmed.chars().take(max_len).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Realistic CWDs that trigger the bug (URL-encoded > 255 bytes).
    const LONG_CWDS: &[&str] = &[
        "/Users/dev/Documents/開発プロジェクト/機能追加/テスト環境/ソースコード/main-branch",
        "/Users/user/Library/Mobile Documents/com~apple~CloudDocs/项目文件/深层嵌套目录/更深层次的/工作区域/project",
        "/Users/user/Library/CloudStorage/OneDrive-대한민국회사/프로젝트/개발환경/소스코드/백엔드/서비스/my-app",
        "/Users/user/Documents/工作文件夹/二零二六年项目/子目录一/子目录二/子目录三/源代码/code",
    ];

    #[test]
    fn long_cwd_uses_hash_fallback_within_name_max() {
        let long_cwd = format!("/Users/test/{}", "中".repeat(30));
        let encoded = encode_cwd_dirname(&long_cwd);
        assert!(encoded.len() <= MAX_DIRNAME_BYTES);
        assert!(!encoded.starts_with("%2F"));
    }

    #[test]
    fn different_long_paths_produce_different_hashes() {
        let a = format!("/Users/test/{}", "中".repeat(30));
        let b = format!("/Users/test/{}", "日".repeat(30));
        assert_ne!(encode_cwd_dirname(&a), encode_cwd_dirname(&b));
    }

    #[test]
    fn decode_reads_cwd_file_for_hash_dirs() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("some-slug-abcdef0123456789");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(".cwd"), "/original/long/path").unwrap();
        assert_eq!(
            decode_cwd_from_dirname(&dir),
            Some("/original/long/path".to_string())
        );
    }

    #[test]
    fn decode_returns_none_without_cwd_file() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("some-slug-abcdef0123456789");
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(decode_cwd_from_dirname(&dir), None);
    }

    #[test]
    fn cwd_file_write_is_idempotent_via_excl() {
        let tmp = TempDir::new().unwrap();
        let long_cwd = format!("/Users/test/{}", "中".repeat(30));
        let dir = tmp.path().join(encode_cwd_dirname(&long_cwd));
        std::fs::create_dir_all(&dir).unwrap();
        let cwd_file = dir.join(".cwd");
        std::fs::write(&cwd_file, &long_cwd).unwrap();
        match std::fs::File::create_new(&cwd_file) {
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            other => panic!("expected AlreadyExists, got: {other:?}"),
        }
        assert_eq!(std::fs::read_to_string(&cwd_file).unwrap(), long_cwd);
    }

    #[test]
    fn url_encoded_long_cwd_fails_on_real_filesystem() {
        let tmp = TempDir::new().unwrap();
        let url_encoded = urlencoding::encode(LONG_CWDS[0]).into_owned();
        let result = std::fs::create_dir_all(tmp.path().join(&url_encoded));
        assert!(result.is_err());
    }

    #[test]
    fn full_roundtrip_on_real_filesystem_for_long_cwds() {
        let tmp = TempDir::new().unwrap();
        for cwd in LONG_CWDS {
            let encoded = encode_cwd_dirname(cwd);
            let dir = tmp.path().join(&encoded);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join(".cwd"), cwd).unwrap();
            assert_eq!(decode_cwd_from_dirname(&dir).as_deref(), Some(*cwd));
        }
    }

    #[test]
    fn short_cwds_use_url_encoding_and_roundtrip_on_real_filesystem() {
        let tmp = TempDir::new().unwrap();
        for cwd in [
            "/Users/foo/project",
            "/tmp",
            "/Users/user/Documents/project-名前",
        ] {
            let encoded = encode_cwd_dirname(cwd);
            assert_eq!(encoded, urlencoding::encode(cwd).into_owned());
            let dir = tmp.path().join(&encoded);
            std::fs::create_dir_all(&dir).unwrap();
            assert_eq!(decode_cwd_from_dirname(&dir).as_deref(), Some(cwd));
        }
    }

    #[cfg(unix)]
    fn unix_mode(path: &std::path::Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[test]
    #[cfg(unix)]
    fn set_dir_owner_only_restricts_mode_to_0700() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("child");
        std::fs::create_dir(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();

        set_dir_owner_only(&dir);

        assert_eq!(unix_mode(&dir), 0o700);
    }

    #[test]
    fn set_dir_owner_only_is_best_effort_on_missing_path() {
        // Must not panic or error — chmod failures are intentionally ignored.
        set_dir_owner_only(std::path::Path::new("/nonexistent/definitely/not/here"));
    }

    #[test]
    #[cfg(unix)]
    fn create_dir_all_owner_only_creates_chain_born_0700() {
        let tmp = TempDir::new().unwrap();
        let leaf = tmp.path().join("a").join("b");
        create_dir_all_owner_only(&leaf).unwrap();
        assert_eq!(unix_mode(&leaf), 0o700, "leaf must be 0700");
        assert_eq!(
            unix_mode(leaf.parent().unwrap()),
            0o700,
            "created intermediate must be born 0700"
        );
    }

    #[test]
    #[cfg(unix)]
    fn create_dir_all_owner_only_retightens_existing_dir() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("existing");
        std::fs::create_dir(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();

        create_dir_all_owner_only(&dir).unwrap();

        assert_eq!(unix_mode(&dir), 0o700);
    }

    #[test]
    #[cfg(unix)]
    fn ensure_sessions_cwd_dir_creates_owner_only_dir_and_root() {
        let home = TempDir::new().unwrap();
        let dir = ensure_sessions_cwd_dir_in(home.path(), "/some/project").unwrap();
        assert!(dir.is_dir());
        assert_eq!(unix_mode(&dir), 0o700);
        assert_eq!(
            unix_mode(&home.path().join("sessions")),
            0o700,
            "sessions root must be 0700 (shields stale children and the search index)"
        );
    }

    #[test]
    #[cfg(unix)]
    fn ensure_sessions_cwd_dir_retightens_existing_loose_dirs() {
        use std::os::unix::fs::PermissionsExt;
        let home = TempDir::new().unwrap();
        let root = home.path().join("sessions");
        let dir = ensure_sessions_cwd_dir_in(home.path(), "/some/project").unwrap();
        // Simulate dirs created by an older echoagent with umask-default perms.
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755)).unwrap();

        let again = ensure_sessions_cwd_dir_in(home.path(), "/some/project").unwrap();

        assert_eq!(again, dir);
        assert_eq!(unix_mode(&dir), 0o700, "mode must self-heal on next touch");
        assert_eq!(unix_mode(&root), 0o700, "root must self-heal on next touch");
    }

    #[test]
    #[cfg(unix)]
    fn ensure_sessions_cwd_dir_hash_encoded_writes_cwd_file_and_owner_only() {
        let home = TempDir::new().unwrap();
        let long_cwd = format!("/Users/test/{}", "中".repeat(30));
        let dir = ensure_sessions_cwd_dir_in(home.path(), &long_cwd).unwrap();
        assert_eq!(unix_mode(&dir), 0o700);
        assert_eq!(std::fs::read_to_string(dir.join(".cwd")).unwrap(), long_cwd);
    }

    #[test]
    fn slugify_basic() {
        assert_eq!(slugify("Hello World!", 40), "hello-world");
    }

    #[test]
    fn slugify_cjk_produces_empty() {
        assert_eq!(slugify("深层目录", 40), "");
    }

    #[test]
    fn slugify_truncates() {
        assert_eq!(slugify(&"a".repeat(100), 10).len(), 10);
    }
}

#[cfg(test)]
mod review_isolation_tests {
    use super::*;

    #[test]
    fn controlled_marker_discovery_stays_in_owned_profile() {
        let root = tempfile::tempdir().unwrap();
        let foreign = root.path().join("synthetic-foreign-home");
        let profile = root.path().join("review-profile");
        for (home, marker) in [(&foreign, "foreign-secret-marker"),
            (&review_discovery_root(Some(profile.clone())), "owned-review-marker")] {
            std::fs::create_dir_all(home.join(".agents/skills/marker")).unwrap();
            std::fs::write(home.join(".agents/skills/marker/SKILL.md"), marker).unwrap();
        }
        let selected = resolve_discovery_home_with(Some(review_discovery_root(Some(profile.clone()))),
            || panic!("isolated discovery invoked normal HOME resolver")).unwrap();
        let normal = resolve_discovery_home_with(None, || Some(foreign.clone())).unwrap();
        assert_eq!(std::fs::read_to_string(normal.join(".agents/skills/marker/SKILL.md")).unwrap(), "foreign-secret-marker");
        assert!(selected.starts_with(&profile));
        assert!(!selected.starts_with(&foreign));
        assert_eq!(std::fs::read_to_string(selected.join(".agents/skills/marker/SKILL.md")).unwrap(), "owned-review-marker");
        assert!(review_discovery_root(Some(PathBuf::from("relative"))).is_absolute());
    }
}
