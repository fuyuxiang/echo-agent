//! Persistent storage for clipboard file blobs.
//!
//! Pasted files do not carry a trustworthy local file path. We land them in
//! `<app_data_dir>/clipboard-images/<unique>/<original-name>` and hand the
//! resulting absolute path back to the frontend, so the existing
//! `attachments: string[]` pipeline (multimodal send, thumbnail render, ACP
//! metadata) keeps working without special-casing the origin.
//!
//! A private per-blob directory preserves the original filename (important for
//! type classification and readable chips) while keeping concurrent pastes
//! collision-free. The accepted extensions deliberately mirror the frontend
//! attachment classifier and the Runtime read_file extractors.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_STORED_ATTACHMENTS: usize = 10_000;
const MAX_STORED_ATTACHMENT_BYTES: u64 = 1024 * 1024 * 1024;

fn fallback_name_for_mime(mime: &str) -> Option<&'static str> {
    match mime.to_ascii_lowercase().as_str() {
        "image/png" => Some("image.png"),
        "image/jpeg" | "image/jpg" => Some("image.jpg"),
        "image/gif" => Some("image.gif"),
        "image/webp" => Some("image.webp"),
        "application/pdf" => Some("document.pdf"),
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document" => {
            Some("document.docx")
        }
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet" => {
            Some("spreadsheet.xlsx")
        }
        "application/vnd.openxmlformats-officedocument.presentationml.presentation" => {
            Some("presentation.pptx")
        }
        "application/vnd.oasis.opendocument.text" => Some("document.odt"),
        "application/vnd.oasis.opendocument.spreadsheet" => Some("spreadsheet.ods"),
        "application/vnd.oasis.opendocument.presentation" => Some("presentation.odp"),
        "application/epub+zip" => Some("book.epub"),
        "application/json" => Some("data.json"),
        "text/markdown" => Some("document.md"),
        "text/yaml" | "text/x-yaml" | "application/yaml" => Some("data.yaml"),
        mime if mime.starts_with("text/") => Some("document.txt"),
        _ => None,
    }
}

const SUPPORTED_EXTENSIONS: &[&str] = &[
    "adoc",
    "astro",
    "bash",
    "bib",
    "c",
    "cc",
    "cfg",
    "cjs",
    "clj",
    "cljc",
    "cljs",
    "conf",
    "cpp",
    "cs",
    "csv",
    "cxx",
    "dart",
    "docx",
    "dockerfile",
    "env",
    "epub",
    "erl",
    "ex",
    "exs",
    "fish",
    "gif",
    "go",
    "gql",
    "gradle",
    "graphql",
    "groovy",
    "h",
    "hh",
    "hpp",
    "hs",
    "ini",
    "java",
    "jpeg",
    "jpg",
    "js",
    "json",
    "json5",
    "jsonc",
    "jsx",
    "kt",
    "kts",
    "log",
    "lua",
    "makefile",
    "markdown",
    "md",
    "mdown",
    "mjs",
    "mk",
    "ml",
    "mli",
    "odp",
    "ods",
    "odt",
    "org",
    "pdf",
    "php",
    "png",
    "pptx",
    "properties",
    "proto",
    "ps1",
    "py",
    "pyi",
    "pyx",
    "r",
    "rb",
    "rs",
    "rst",
    "sc",
    "scala",
    "sh",
    "sql",
    "svelte",
    "swift",
    "tex",
    "toml",
    "ts",
    "tsv",
    "tsx",
    "txt",
    "vue",
    "webp",
    "xlsx",
    "xml",
    "xsd",
    "xsl",
    "yaml",
    "yml",
    "zsh",
];

const SUPPORTED_EXACT_NAMES: &[&str] = &[
    ".editorconfig",
    ".env.example",
    ".gitattributes",
    ".gitignore",
    "cmakelists.txt",
    "dockerfile",
    "makefile",
    "rakefile",
];

pub(crate) fn is_supported_attachment_path(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
        return false;
    };
    let lower = name.to_ascii_lowercase();
    if SUPPORTED_EXACT_NAMES.contains(&lower.as_str()) {
        return true;
    }
    path.extension()
        .and_then(|value| value.to_str())
        .map(str::to_ascii_lowercase)
        .is_some_and(|extension| SUPPORTED_EXTENSIONS.contains(&extension.as_str()))
}

/// Build a safe base filename from the suggested name. Strips path
/// separators and bounds the filename while retaining supported extensions.
fn sanitize_basename(suggested: Option<&str>) -> Option<String> {
    let raw = suggested?.trim();
    if raw.is_empty() {
        return None;
    }
    // Strip directory components first so traversal attempts and absolute
    // paths collapse to their basename segment ("../../../etc/passwd" →
    // "passwd", "C:\\tmp\\foo.png" → "foo.png").
    let last_sep = raw.rfind(['/', '\\']).map(|index| index + 1).unwrap_or(0);
    let tail = &raw[last_sep..];
    // Then sanitize remaining OS-unsafe characters inside the basename.
    let cleaned: String = tail
        .chars()
        .map(|ch| match ch {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    // Leading dots are meaningful for `.gitignore` / `.editorconfig`. Only
    // trailing dots/spaces are invalid on Windows; dot-only names are rejected.
    let trimmed = cleaned.trim().trim_end_matches(['.', ' ']).to_string();
    if trimmed.is_empty() || trimmed == "." || trimmed == ".." {
        return None;
    }
    let name = Path::new(&trimmed);
    let extension = name
        .extension()
        .and_then(|extension| extension.to_str())
        .filter(|_| is_supported_attachment_path(name));
    let (stem, suffix) = extension.map_or((trimmed.as_str(), ""), |extension| {
        trimmed.split_at(trimmed.len() - extension.len() - 1)
    });
    let mut bounded: String = stem.chars().take(80 - suffix.len()).collect();
    while bounded.len() + suffix.len() > 255 {
        bounded.pop();
    }
    bounded.push_str(suffix);
    Some(bounded)
}

/// Monotonic + 6-hex-char suffix; combines nanos + epoch nanos fallback to
/// stay unique even within the same nanosecond under contention.
fn unique_suffix() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let nanos = now.as_nanos();
    // Cheap pseudo-random tail; we don't need cryptographic strength, only
    // collision avoidance for concurrent paste events in the same nanosecond.
    let pid = std::process::id();
    let rand = pid.rotate_left(7) ^ nanos as u32 ^ (nanos >> 32) as u32;
    format!("{:013}-{:06x}", nanos, rand & 0xFF_FFFF)
}

/// Compose the destination path. The caller is responsible for creating
/// `dir` if it does not already exist (we keep this function pure for tests).
pub(crate) fn build_destination(
    dir: &Path,
    mime: &str,
    suggested_name: Option<&str>,
) -> Result<PathBuf, String> {
    let suggested = sanitize_basename(suggested_name);
    let file_name = suggested
        .filter(|name| is_supported_attachment_path(Path::new(name)))
        .or_else(|| fallback_name_for_mime(mime).map(str::to_string))
        .ok_or_else(|| format!("不支持的附件类型：{mime}"))?;
    Ok(dir.join(unique_suffix()).join(file_name))
}

/// Bound the application-owned paste store without coupling it to the much
/// smaller process-lifetime exact-file grant set. Oldest blobs are evicted
/// first; `keep` protects the file just written by the current operation.
pub(crate) fn prune_store(dir: &Path, keep: Option<&Path>) -> Result<(), String> {
    prune_store_with_limits(
        dir,
        keep,
        MAX_STORED_ATTACHMENTS,
        MAX_STORED_ATTACHMENT_BYTES,
    )
}

fn prune_store_with_limits(
    dir: &Path,
    keep: Option<&Path>,
    max_count: usize,
    max_bytes: u64,
) -> Result<(), String> {
    let directory = match crate::paths::canonical_managed_dir(dir) {
        Ok(path) => path,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("读取附件目录失败：{error}")),
    };
    let dir = directory.as_path();
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("读取附件目录失败：{error}")),
    };
    let keep = keep.and_then(|path| path.canonicalize().ok());
    let mut files: Vec<(PathBuf, u64, SystemTime)> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(metadata) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        let candidates = if metadata.file_type().is_symlink() {
            Vec::new()
        } else if metadata.is_dir() {
            std::fs::read_dir(&path)
                .map(|children| children.flatten().map(|child| child.path()).collect())
                .unwrap_or_default()
        } else {
            vec![path]
        };
        for candidate in candidates {
            let Ok(metadata) = std::fs::symlink_metadata(&candidate) else {
                continue;
            };
            if metadata.file_type().is_symlink()
                || !metadata.is_file()
                || !is_supported_attachment_path(&candidate)
            {
                continue;
            }
            files.push((
                candidate,
                metadata.len(),
                metadata.modified().unwrap_or(UNIX_EPOCH),
            ));
        }
    }
    files.sort_by_key(|(_, _, modified)| *modified);
    let mut total_bytes = files
        .iter()
        .fold(0_u64, |total, (_, size, _)| total.saturating_add(*size));
    let mut total_count = files.len();
    for (path, size, _) in files {
        if total_count <= max_count && total_bytes <= max_bytes {
            break;
        }
        if keep.as_ref().is_some_and(|kept| {
            path.canonicalize()
                .is_ok_and(|candidate| candidate.as_path() == kept.as_path())
        }) {
            continue;
        }
        match std::fs::remove_file(&path) {
            Ok(()) => {
                total_count = total_count.saturating_sub(1);
                total_bytes = total_bytes.saturating_sub(size);
                if let Some(parent) = path.parent() {
                    if parent != dir {
                        let _ = std::fs::remove_dir(parent);
                    }
                }
            }
            Err(error) => {
                tracing::warn!(path = %path.display(), %error, "failed to evict old attachment blob");
            }
        }
    }
    Ok(())
}

/// Write bytes to a fresh file inside `dir` using the private-file helper so
/// permissions stay owner-only on unix. Returns the absolute path.
pub fn save_blob(
    dir: &Path,
    bytes: &[u8],
    mime: &str,
    suggested_name: Option<&str>,
) -> Result<PathBuf, String> {
    if bytes.is_empty() {
        return Err("粘贴的文件为空".into());
    }
    if bytes.len() as u64 > crate::shell_fs::MAX_ATTACHMENT_FILE_BYTES {
        return Err(format!(
            "单个附件不能超过 {}MB",
            crate::shell_fs::MAX_ATTACHMENT_FILE_BYTES / 1024 / 1024
        ));
    }
    let destination = build_destination(dir, mime, suggested_name)?;
    std::fs::create_dir_all(dir)
        .map_err(|error| format!("创建附件目录 {} 失败：{error}", dir.display()))?;
    let directory = crate::paths::canonical_managed_dir(dir)
        .map_err(|error| format!("解析附件目录失败：{error}"))?;
    let relative = destination
        .strip_prefix(dir)
        .map_err(|error| format!("解析附件路径失败：{error}"))?;
    let destination = directory.join(relative);
    let parent = destination
        .parent()
        .ok_or_else(|| "无法创建附件目录".to_string())?;
    std::fs::create_dir(parent)
        .map_err(|error| format!("创建附件目录 {} 失败：{error}", parent.display()))?;
    crate::paths::write_private_file(&destination, bytes)?;
    // write_private_file uses a staging file inside the same dir; the
    // canonicalize-to-staging dance is irrelevant to the caller.
    std::fs::canonicalize(&destination).map_err(|error| format!("解析附件路径失败：{error}"))
}

#[tauri::command]
pub async fn save_attachment_blob(
    app: tauri::AppHandle,
    access: tauri::State<'_, crate::shell_fs::FilesystemAccess>,
    bytes: Vec<u8>,
    mime: String,
    suggested_name: Option<String>,
) -> Result<String, String> {
    let dir = crate::paths::desktop_data_dir(&app)
        .map_err(|error| format!("解析应用数据目录失败：{error}"))?
        .join("clipboard-images");
    let path = save_blob(&dir, &bytes, &mime, suggested_name.as_deref())?;
    if let Err(error) = access.require_managed_attachment(&path) {
        let _ = std::fs::remove_file(&path);
        if let Some(parent) = path.parent() {
            let _ = std::fs::remove_dir(parent);
        }
        return Err(error);
    }
    if let Err(error) = prune_store(&dir, Some(&path)) {
        tracing::warn!(dir = %dir.display(), %error, "failed to prune attachment store after save");
    }
    Ok(path.to_string_lossy().into_owned())
}

#[tauri::command]
pub async fn discard_attachment_blob(
    app: tauri::AppHandle,
    access: tauri::State<'_, crate::shell_fs::FilesystemAccess>,
    path: String,
) -> Result<(), String> {
    let dir = crate::paths::desktop_data_dir(&app)
        .map_err(|error| format!("解析应用数据目录失败：{error}"))?
        .join("clipboard-images");
    let raw = PathBuf::from(path);
    let Ok(metadata) = std::fs::symlink_metadata(&raw) else {
        return Ok(());
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Ok(());
    }
    let Ok(canonical_dir) = crate::paths::canonical_managed_dir(&dir) else {
        return Ok(());
    };
    let Ok(canonical) = std::fs::canonicalize(&raw) else {
        return Ok(());
    };
    if !canonical.starts_with(&canonical_dir) {
        return Ok(());
    }
    std::fs::remove_file(&canonical).map_err(|error| format!("删除未发送附件失败：{error}"))?;
    if let Some(parent) = canonical.parent() {
        if parent != canonical_dir {
            let _ = std::fs::remove_dir(parent);
        }
    }
    access.revoke_authorized_file(&canonical)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_read(path: &Path) -> Vec<u8> {
        std::fs::read(path).expect("read back saved blob")
    }

    #[test]
    fn writes_bytes_to_disk_and_returns_absolute_path() {
        let temp = tempfile::tempdir().expect("temp dir");
        let bytes = b"\x89PNG\r\n\x1a\nfake-image-bytes";

        let path = save_blob(temp.path(), bytes, "image/png", None).expect("save");
        assert!(path.is_absolute(), "saved path must be absolute: {path:?}");
        assert_eq!(write_read(&path), bytes);
    }

    #[test]
    fn extension_follows_mime_mapping_for_known_image_types() {
        let temp = tempfile::tempdir().expect("temp dir");
        let png = save_blob(temp.path(), b"x", "image/png", None).unwrap();
        assert_eq!(png.extension().and_then(|v| v.to_str()), Some("png"));
        let jpeg = save_blob(temp.path(), b"x", "image/jpeg", None).unwrap();
        assert_eq!(jpeg.extension().and_then(|v| v.to_str()), Some("jpg"));
        let gif = save_blob(temp.path(), b"x", "image/gif", None).unwrap();
        assert_eq!(gif.extension().and_then(|v| v.to_str()), Some("gif"));
        let webp = save_blob(temp.path(), b"x", "image/webp", None).unwrap();
        assert_eq!(webp.extension().and_then(|v| v.to_str()), Some("webp"));
    }

    #[test]
    fn unknown_attachment_mime_is_rejected() {
        let temp = tempfile::tempdir().expect("temp dir");
        let error = save_blob(temp.path(), b"x", "image/avif", None).unwrap_err();
        assert!(error.contains("不支持的附件类型"));
    }

    #[test]
    fn mime_is_case_insensitive() {
        let temp = tempfile::tempdir().expect("temp dir");
        let path = save_blob(temp.path(), b"x", "IMAGE/PNG", None).unwrap();
        assert_eq!(path.extension().and_then(|v| v.to_str()), Some("png"));
    }

    #[test]
    fn suggested_name_is_used_as_stem_when_safe() {
        let temp = tempfile::tempdir().expect("temp dir");
        let path = save_blob(temp.path(), b"x", "image/png", Some("screenshot.png")).unwrap();
        let file_name = path.file_name().unwrap().to_string_lossy().to_string();
        assert_eq!(file_name, "screenshot.png");
    }

    #[test]
    fn suggested_name_strips_path_separators_and_traversal() {
        let temp = tempfile::tempdir().expect("temp dir");
        // 路径穿越和绝对路径都只应留下 basename 段。
        let evil = "../../../etc/passwd.png";
        let path = save_blob(temp.path(), b"x", "image/png", Some(evil)).unwrap();
        let file_name = path.file_name().unwrap().to_string_lossy().to_string();
        assert_eq!(file_name, "passwd.png");
        // 必须仍在 temp 目录内(不能逃逸)。
        assert!(
            path.starts_with(temp.path().canonicalize().unwrap()),
            "{path:?}"
        );
    }

    #[test]
    fn suggested_name_sanitizes_os_unsafe_characters() {
        let temp = tempfile::tempdir().expect("temp dir");
        let path = save_blob(temp.path(), b"x", "image/png", Some("a:b*c?d\"e<f>g|h.png")).unwrap();
        let file_name = path.file_name().unwrap().to_string_lossy().to_string();
        assert_eq!(file_name, "a_b_c_d_e_f_g_h.png", "got {file_name}");
    }

    #[test]
    fn empty_or_whitespace_suggested_name_falls_back_to_image() {
        let temp = tempfile::tempdir().expect("temp dir");
        for bad in [None, Some(""), Some("   "), Some("....."), Some("\t\n")] {
            let path = save_blob(temp.path(), b"x", "image/png", bad).unwrap();
            let file_name = path.file_name().unwrap().to_string_lossy().to_string();
            assert!(file_name == "image.png", "got {file_name} for {bad:?}");
        }
    }

    #[test]
    fn two_concurrent_saves_get_distinct_filenames() {
        let temp = tempfile::tempdir().expect("temp dir");
        let a = save_blob(temp.path(), b"abc", "image/png", None).unwrap();
        let b = save_blob(temp.path(), b"abc", "image/png", None).unwrap();
        assert_ne!(a, b, "concurrent saves must not collide");
        assert_eq!(write_read(&a), b"abc");
        assert_eq!(write_read(&b), b"abc");
    }

    #[test]
    fn empty_bytes_is_rejected_before_disk_write() {
        let temp = tempfile::tempdir().expect("temp dir");
        let error = save_blob(temp.path(), b"", "image/png", None).expect_err("empty blob");
        assert!(error.contains("空"), "got {error}");
        assert_eq!(
            std::fs::read_dir(temp.path()).unwrap().count(),
            0,
            "no file should be created on rejection"
        );
    }

    #[test]
    fn oversized_blob_is_rejected_before_disk_write() {
        let temp = tempfile::tempdir().expect("temp dir");
        let destination = temp.path().join("clipboard-images");
        let bytes = vec![0; crate::shell_fs::MAX_ATTACHMENT_FILE_BYTES as usize + 1];

        let error = save_blob(&destination, &bytes, "image/png", None)
            .expect_err("oversized blob must be rejected");

        assert!(error.contains("20MB"), "got {error}");
        assert!(
            !destination.exists(),
            "rejected blob must not create its destination directory"
        );
    }

    #[test]
    fn creates_directory_when_missing() {
        let temp = tempfile::tempdir().expect("temp dir");
        let nested = temp.path().join("does/not/exist/yet");
        let path = save_blob(&nested, b"x", "image/png", None).unwrap();
        assert!(path.exists());
        assert_eq!(write_read(&path), b"x");
    }

    #[test]
    fn long_suggested_name_preserves_the_extension_with_portable_limits() {
        let temp = tempfile::tempdir().expect("temp dir");
        for stem in ["a".repeat(500), "经营📑".repeat(100), "📑".repeat(100)] {
            let long = format!("{stem}.XLSX");
            let path = save_blob(
                temp.path(),
                b"spreadsheet bytes",
                "application/octet-stream",
                Some(&long),
            )
            .unwrap();
            let file_name = path.file_name().unwrap().to_str().unwrap();
            let kept = file_name.strip_suffix(".XLSX").unwrap();
            assert!(!kept.is_empty() && stem.starts_with(kept));
            assert!(file_name.len() <= 255 && file_name.chars().count() <= 80);
            assert_eq!(write_read(&path), b"spreadsheet bytes");
        }
    }

    #[test]
    fn preserves_supported_non_image_names_and_special_dotfiles() {
        let temp = tempfile::tempdir().unwrap();
        for (name, mime) in [
            ("report.pdf", "application/pdf"),
            ("notes.md", "text/markdown"),
            ("service.dockerfile", "text/plain"),
            ("rules.makefile", "text/plain"),
            (
                "table.xlsx",
                "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
            ),
            (".gitignore", "text/plain"),
        ] {
            let path = save_blob(temp.path(), b"content", mime, Some(name)).unwrap();
            assert_eq!(
                path.file_name().and_then(|value| value.to_str()),
                Some(name)
            );
            assert!(is_supported_attachment_path(&path));
        }
    }

    #[test]
    fn prune_store_evicts_oldest_and_keeps_current_blob() {
        let temp = tempfile::tempdir().unwrap();
        let first = save_blob(temp.path(), b"1111", "text/plain", Some("first.txt")).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
        let second = save_blob(temp.path(), b"2222", "text/plain", Some("second.txt")).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
        let newest = save_blob(temp.path(), b"3333", "text/plain", Some("newest.txt")).unwrap();

        prune_store_with_limits(temp.path(), Some(&newest), 2, 8).unwrap();

        assert!(!first.exists(), "oldest blob should be evicted first");
        assert!(second.exists());
        assert!(newest.exists(), "current save must never be evicted");
    }

    #[cfg(unix)]
    #[test]
    fn linked_attachment_root_is_not_written_or_pruned() {
        let data_home = tempfile::tempdir().unwrap();
        let external = tempfile::tempdir().unwrap();
        let attachment_root = data_home.path().join("clipboard-images");
        let existing = external.path().join("notes.txt");
        std::fs::write(&existing, b"external notes").unwrap();
        std::os::unix::fs::symlink(external.path(), &attachment_root).unwrap();

        assert!(save_blob(&attachment_root, b"pasted", "text/plain", Some("paste.txt")).is_err());
        assert!(prune_store_with_limits(&attachment_root, None, 0, 0).is_err());
        assert_eq!(std::fs::read(&existing).unwrap(), b"external notes");
        assert_eq!(std::fs::read_dir(external.path()).unwrap().count(), 1);
    }
}
