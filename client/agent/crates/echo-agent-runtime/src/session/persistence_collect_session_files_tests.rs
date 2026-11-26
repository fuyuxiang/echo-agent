use super::*;
use std::fs;
use tempfile::TempDir;

#[test]
fn collects_top_level_files_with_flat_names() {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("chat_history.jsonl"), b"line1\nline2").unwrap();
    fs::write(dir.path().join("summary.json"), b"{}").unwrap();

    let mut files = Vec::new();
    collect_session_files_recursive(dir.path(), dir.path(), &mut files);

    files.sort_by(|a, b| a.name.cmp(&b.name));
    assert_eq!(files.len(), 2);
    assert_eq!(files[0].name, "chat_history.jsonl");
    assert_eq!(files[0].data, b"line1\nline2");
    assert_eq!(files[1].name, "summary.json");
    assert_eq!(files[1].data, b"{}");
}

#[test]
fn collects_subdirectory_files_with_relative_paths() {
    let dir = TempDir::new().unwrap();
    let prompts_dir = dir.path().join("prompts");
    fs::create_dir(&prompts_dir).unwrap();
    fs::write(prompts_dir.join("prompt_0.txt"), b"long prompt content").unwrap();
    fs::write(prompts_dir.join("prompt_1.txt"), b"another long prompt").unwrap();
    fs::write(dir.path().join("summary.json"), b"{}").unwrap();

    let mut files = Vec::new();
    collect_session_files_recursive(dir.path(), dir.path(), &mut files);

    files.sort_by(|a, b| a.name.cmp(&b.name));
    assert_eq!(files.len(), 3);
    assert_eq!(files[0].name, "prompts/prompt_0.txt");
    assert_eq!(files[0].data, b"long prompt content");
    assert_eq!(files[1].name, "prompts/prompt_1.txt");
    assert_eq!(files[2].name, "summary.json");
}

#[test]
fn collects_nested_subdirectories() {
    let dir = TempDir::new().unwrap();
    let deep = dir.path().join("a").join("b");
    fs::create_dir_all(&deep).unwrap();
    fs::write(deep.join("deep.txt"), b"deep").unwrap();
    fs::write(dir.path().join("top.txt"), b"top").unwrap();

    let mut files = Vec::new();
    collect_session_files_recursive(dir.path(), dir.path(), &mut files);

    files.sort_by(|a, b| a.name.cmp(&b.name));
    assert_eq!(files.len(), 2);
    assert_eq!(files[0].name, "a/b/deep.txt");
    assert_eq!(files[1].name, "top.txt");
}

#[test]
fn nonexistent_directory_returns_empty() {
    let dir = TempDir::new().unwrap();
    let missing = dir.path().join("does_not_exist");

    let mut files = Vec::new();
    collect_session_files_recursive(&missing, &missing, &mut files);

    assert!(files.is_empty());
}

#[test]
fn empty_directory_returns_empty() {
    let dir = TempDir::new().unwrap();

    let mut files = Vec::new();
    collect_session_files_recursive(dir.path(), dir.path(), &mut files);

    assert!(files.is_empty());
}

#[test]
fn skips_empty_subdirectories() {
    let dir = TempDir::new().unwrap();
    fs::create_dir(dir.path().join("empty_subdir")).unwrap();
    fs::write(dir.path().join("file.txt"), b"data").unwrap();

    let mut files = Vec::new();
    collect_session_files_recursive(dir.path(), dir.path(), &mut files);

    assert_eq!(files.len(), 1);
    assert_eq!(files[0].name, "file.txt");
}
