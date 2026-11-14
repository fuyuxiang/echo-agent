//! Read-only outcome verification for an explicit output path in a user task.
//! This never creates, renames or writes the document; the application must save it.
use serde_json::{json, Value};
use std::{
    io::Read,
    path::{Component, Path, PathBuf},
};
/// Explicit end-of-text representation for providers that trim string edges.
/// Without this option text is preserved byte-for-byte; no newline is inferred.
pub fn with_trailing_newlines(value: &str, count: Option<u64>) -> Result<String, String> {
    let Some(count) = count else {
        return Ok(value.to_owned());
    };
    if count > 16 {
        return Err("invalid_argument: trailing newline count must be at most 16".into());
    }
    Ok(format!(
        "{}{}",
        value.trim_end_matches('\n'),
        "\n".repeat(count as usize)
    ))
}

/// Trusted host locations, never model-provided directory mappings.
#[derive(Default)]
pub struct Locations {
    pub home: Option<PathBuf>,
    pub desktop: Option<PathBuf>,
    pub documents: Option<PathBuf>,
    pub downloads: Option<PathBuf>,
    pub workspace: Option<PathBuf>,
}

fn same_path(a: &Path, b: &Path) -> bool {
    #[cfg(target_os = "windows")]
    {
        a.to_string_lossy().replace('\\', "/").to_lowercase()
            == b.to_string_lossy().replace('\\', "/").to_lowercase()
    }
    #[cfg(not(target_os = "windows"))]
    {
        a == b
    }
}

fn resolve_mention(text: &str, locations: &Locations) -> Option<PathBuf> {
    let path = Path::new(text);
    if path.components().any(|c| c == Component::ParentDir) {
        return None;
    }
    if path.is_absolute() {
        return Some(path.to_owned());
    }
    if let Some(tail) = text.strip_prefix("~/").or_else(|| text.strip_prefix("~\\")) {
        return locations.home.as_ref().map(|home| home.join(tail));
    }
    for (labels, base) in [
        (&["桌面/", "desktop/"][..], &locations.desktop),
        (&["文稿/", "文档/", "documents/"][..], &locations.documents),
        (&["下载/", "downloads/"][..], &locations.downloads),
        (&["工作区/", "workspace/"][..], &locations.workspace),
    ] {
        for label in labels {
            if text.to_lowercase().starts_with(label) {
                return base.as_ref().map(|base| base.join(&text[label.len()..]));
            }
        }
    }
    None
}

fn mentions(scope: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    // A path supplied as a whole answer can contain spaces without quotes.
    for line in scope.lines() {
        let line = line.trim();
        let location = [
            "路径为",
            "路径是",
            "目录为",
            "目录是",
            "保存位置为",
            "保存位置是",
        ]
        .iter()
        .find_map(|prefix| line.strip_prefix(prefix))
        .unwrap_or(line)
        .trim();
        if let Some(path) = super::task::literal_path(location) {
            tokens.push(path.to_owned());
        }
        if let Some(name) = super::task::output_name(line) {
            tokens.push(name.to_owned());
        }
    }
    let mut quoted = None;
    let mut token = String::new();
    for ch in scope.chars().chain(std::iter::once(' ')) {
        if let Some(end) = quoted {
            if ch == end {
                tokens.push(std::mem::take(&mut token));
                quoted = None;
            } else {
                token.push(ch);
            }
        } else if matches!(ch, '`' | '\"' | '\'' | '“' | '‘') {
            if !token.is_empty() {
                tokens.push(std::mem::take(&mut token));
            }
            quoted = Some(match ch {
                '“' => '”',
                '‘' => '’',
                ch => ch,
            });
        } else if ch.is_whitespace()
            || matches!(
                ch,
                '，' | ',' | '。' | '；' | ';' | '！' | '!' | '（' | '(' | '）' | ')' | '：'
            )
        {
            if !token.is_empty() {
                tokens.push(std::mem::take(&mut token));
            }
        } else {
            token.push(ch);
        }
    }
    tokens
}

pub fn path_in_scope(scope: Option<&str>, path: &Path, locations: &Locations) -> bool {
    let Some(scope) = scope else { return false };
    let tokens = mentions(scope);
    if tokens.iter().any(|token| {
        let token = token.trim_end_matches(['.', '。']);
        // Chinese instructions often attach the preposition directly to a path.
        if token.contains("://") {
            return false;
        }
        token.char_indices().any(|(i, _)| {
            let prefix = &token[..i];
            let boundary = i == 0
                || [
                    "保存到",
                    "保存为",
                    "存到",
                    "输出到",
                    "路径为",
                    "路径是",
                    "目录为",
                    "目录是",
                    "位置为",
                    "位置是",
                    "改为",
                ]
                .iter()
                .any(|p| prefix.ends_with(p));
            boundary
                && resolve_mention(&token[i..], locations)
                    .is_some_and(|candidate| same_path(&candidate, path))
        })
    }) {
        return true;
    }
    // "保存到桌面，文件名为 report.txt" is also an explicit location. Match
    // the exact basename, not a prefix or a neighboring file in that folder.
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    let named = tokens.iter().any(|token| {
        token.trim_end_matches('.') == name
            || ["文件名为", "文件名是", "命名为"]
                .iter()
                .any(|prefix| token.strip_prefix(prefix).is_some_and(|v| v == name))
    });
    named
        && [
            (&["桌面", "desktop"][..], &locations.desktop),
            (&["文稿", "文档", "documents"][..], &locations.documents),
            (&["下载", "downloads"][..], &locations.downloads),
            (&["工作区", "workspace"][..], &locations.workspace),
        ]
        .iter()
        .any(|(labels, base)| {
            base.as_ref()
                .is_some_and(|base| path.parent().is_some_and(|parent| same_path(parent, base)))
                && labels
                    .iter()
                    .any(|label| scope.to_lowercase().contains(label))
        })
}

#[cfg(test)]
pub fn verify_file(
    scope: Option<&str>,
    path: &str,
    expected: Option<&str>,
) -> Result<Value, String> {
    verify_file_at(scope, path, expected, &Locations::default())
}

pub fn verify_file_at(
    scope: Option<&str>,
    path: &str,
    expected: Option<&str>,
    locations: &Locations,
) -> Result<Value, String> {
    let target = Path::new(path);
    if !target.is_absolute()
        || target.components().any(|c| c == Component::ParentDir)
        || !path_in_scope(scope, target, locations)
    {
        return Err("scope_required: verify only an explicit absolute output path from the user's current task".into());
    }
    let metadata = match std::fs::symlink_metadata(target) {
        Ok(metadata) => metadata,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(
                json!({"path":path,"exists":false,"verified":false,"status":"file_not_found"}),
            )
        }
        Err(e) => return Err(format!("file_unavailable: {e}")),
    };
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err("unsupported_target: expected a regular output file".into());
    }
    if expected.is_some() && metadata.len() > 1024 * 1024 {
        return Err("verification_limit: output exceeds 1 MiB readback limit".into());
    }
    let mut result = json!({"path":path,"exists":true,"bytes":metadata.len(),"locationVerified":true,"verified":true,"status":"verified_file_exists"});
    if let Some(expected) = expected {
        let mut bytes = Vec::new();
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        }
        let file = options
            .open(target)
            .map_err(|e| format!("file_unavailable: {e}"))?;
        if !file
            .metadata()
            .map_err(|e| format!("file_unavailable: {e}"))?
            .is_file()
        {
            return Err("unsupported_target: expected a regular output file".into());
        }
        file.take(1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| format!("file_unavailable: {e}"))?;
        if bytes.len() > 1024 * 1024 {
            return Err("verification_limit: output exceeds 1 MiB readback limit".into());
        }
        let after = std::fs::metadata(target).map_err(|e| format!("file_unavailable: {e}"))?;
        if after.len() != metadata.len() || after.modified().ok() != metadata.modified().ok() {
            return Ok(
                json!({"path":path,"exists":true,"verified":false,"status":"file_changed_during_readback"}),
            );
        }
        let text = std::str::from_utf8(&bytes).map_err(|_| {
            "unsupported_encoding: exact text verification requires UTF-8".to_string()
        })?;
        let matches = text == expected;
        result["characters"] = json!(text.chars().count());
        result["expectedCharacters"] = json!(expected.chars().count());
        result["actualTrailingNewlines"] =
            json!(text.chars().rev().take_while(|c| *c == '\n').count());
        result["expectedTrailingNewlines"] =
            json!(expected.chars().rev().take_while(|c| *c == '\n').count());
        if !matches {
            result["hint"]=json!("Compare expectedCharacters and trailing newline counts with the original request. For boundary-sensitive text, pass expectedTrailingNewlines explicitly; do not alter an already-correct document to fit a malformed expectation.");
        }

        result["textMatchesExpected"] = json!(matches);
        result["verified"] = json!(matches);
        result["status"] = json!(if matches {
            "verified_exact_text"
        } else {
            "text_mismatch"
        });
    }
    Ok(result)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn missing_file_is_not_success_and_unrequested_paths_are_rejected() {
        let path = std::env::temp_dir().join(format!("echo-artifact-{}", uuid::Uuid::now_v7()));
        let path = path.to_str().unwrap();
        assert!(verify_file(Some("another task"), path, None).is_err());
        let result = verify_file(Some(path), path, None).unwrap();
        assert_eq!(result["exists"], false);
        assert_eq!(result["verified"], false);
    }
    #[test]
    fn exact_unicode_and_final_newline_are_verified_without_writes() {
        let path = std::env::temp_dir().join(format!("echo-artifact-{}", uuid::Uuid::now_v7()));
        let p = path.to_str().unwrap();
        std::fs::write(&path, "中文\n多行\n").unwrap();
        assert_eq!(
            verify_file(Some(p), p, Some("中文\n多行\n")).unwrap()["verified"],
            true
        );
        assert_eq!(
            verify_file(Some(p), p, Some("中文\n多行")).unwrap()["verified"],
            false
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "中文\n多行\n");
        std::fs::remove_file(path).unwrap();
    }
}

#[cfg(test)]
mod text_ending_tests {
    use super::*;
    #[test]
    fn explicit_terminal_newlines_preserve_default_and_override_only_lf_endings() {
        assert_eq!(
            with_trailing_newlines("中文\n内容  ", None).unwrap(),
            "中文\n内容  "
        );
        assert_eq!(
            with_trailing_newlines("中文\n内容", Some(1)).unwrap(),
            "中文\n内容\n"
        );
        assert_eq!(
            with_trailing_newlines("中文\n内容\n\n", Some(1)).unwrap(),
            "中文\n内容\n"
        );
        assert!(with_trailing_newlines("text", Some(17)).is_err());
    }
}

#[cfg(test)]
mod scope_tests {
    use super::*;
    #[test]
    fn subsequent_user_path_and_named_folder_resolve_without_granting_neighbors() {
        let root = std::env::temp_dir().join(format!("echo-task-path-{}", uuid::Uuid::now_v7()));
        let locations = Locations {
            home: Some(root.clone()),
            desktop: Some(root.join("Desktop")),
            workspace: Some(root.join("workspace")),
            ..Default::default()
        };
        let target = root.join("Desktop").join("report.txt");
        for scope in [
            "保存到 ~/Desktop/report.txt",
            "保存到桌面/report.txt",
            "保存到桌面，文件名为 report.txt",
        ] {
            assert!(path_in_scope(Some(scope), &target, &locations), "{scope}");
            assert!(!path_in_scope(
                Some(scope),
                &root.join("Desktop/other.txt"),
                &locations
            ));
        }
        let path = root.join("new output.txt");
        let original = "请在 TextEdit 新建文稿并保存";
        let update = format!("把它保存到 \"{}\"", path.display());
        let scope = super::super::task::updated_scope(Some(original), true, &update).unwrap();
        assert!(path_in_scope(Some(&scope), &path, &locations));
        assert!(!path_in_scope(Some(original), &path, &locations));
        let only_longer = format!("{}.bak", path.display());
        assert!(!path_in_scope(Some(&only_longer), &path, &locations));
    }
    #[test]
    fn successive_location_and_filename_answers_verify_the_saved_file_read_only() {
        use super::super::task::updated_scope;
        let root = std::env::temp_dir().join(format!("echo-answer-path-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir(&root).unwrap();
        let path = root.join("New Report.txt");
        let expected = "中文\n最终正文\n";
        std::fs::write(&path, expected).unwrap();
        let locations = Locations {
            desktop: Some(root.clone()),
            ..Default::default()
        };
        let mut scope = "请在 TextEdit 新建文稿并保存".to_owned();
        for answer in ["桌面", "文件名为 New Report.txt。"] {
            scope = updated_scope(Some(&scope), true, answer).unwrap();
        }
        assert!(verify_file_at(
            Some(&scope),
            path.to_str().unwrap(),
            Some(expected),
            &locations
        )
        .unwrap()["verified"]
            .as_bool()
            .unwrap());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), expected);
        let labeled_path = format!("路径为 {}", path.display());
        assert!(path_in_scope(Some(&labeled_path), &path, &locations));
        std::fs::remove_file(&path).unwrap();
        std::fs::remove_dir(root).unwrap();
    }

    #[test]
    fn relative_parent_traversal_and_url_substrings_do_not_authorize_output_paths() {
        let root = std::env::temp_dir();
        let locations = Locations {
            home: Some(root.clone()),
            ..Default::default()
        };
        assert!(!path_in_scope(
            Some("保存到 ~/../secret.txt"),
            &root.join("secret.txt"),
            &locations
        ));
        assert!(!path_in_scope(
            Some("https://example.com/secret.txt"),
            Path::new("/secret.txt"),
            &locations
        ));
    }
}
