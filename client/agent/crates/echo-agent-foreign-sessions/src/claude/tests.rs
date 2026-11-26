use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use filetime::FileTime;
use serde_json::json;
use tempfile::TempDir;

use super::*;
use crate::MAX_SESSION_AGE;

fn set_mtime(path: &Path, time: SystemTime) {
    filetime::set_file_mtime(path, FileTime::from_system_time(time)).unwrap();
}

fn write_session(
    project: &Path,
    id: uuid::Uuid,
    lines: &[serde_json::Value],
    modified: SystemTime,
) {
    fs::create_dir_all(project).unwrap();
    let path = project.join(format!("{id}.jsonl"));
    let contents = lines
        .iter()
        .map(serde_json::Value::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    fs::write(&path, format!("{contents}\n")).unwrap();
    set_mtime(&path, modified);
}

fn user(cwd: &Path, prompt: &str) -> serde_json::Value {
    json!({
        "type": "user",
        "cwd": cwd,
        "message": {"role": "user", "content": prompt}
    })
}

fn scoped_project(root: &Path, cwd: &Path) -> std::path::PathBuf {
    let name = cwd
        .to_string_lossy()
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
        .collect::<String>();
    let path = root.join("projects").join(name);
    fs::create_dir_all(&path).unwrap();
    path
}

#[test]
fn recent_probe_skips_sidechains_and_wrong_cwds_before_winner() {
    let root = TempDir::new().unwrap();
    let cwd = root.path().join("repo");
    fs::create_dir_all(&cwd).unwrap();
    let project = scoped_project(root.path(), &cwd);
    let now = UNIX_EPOCH + Duration::from_secs(3_000_000);
    write_session(
        &project,
        uuid::Uuid::from_u128(10),
        &[json!({
            "type": "user",
            "cwd": cwd,
            "isSidechain": true,
            "message": {"content": "sidechain"}
        })],
        now,
    );
    write_session(
        &project,
        uuid::Uuid::from_u128(11),
        &[user(Path::new("/other"), "wrong cwd")],
        now - Duration::from_secs(1),
    );
    let winner = uuid::Uuid::from_u128(12);
    write_session(
        &project,
        winner,
        &[user(&cwd, "winner")],
        now - Duration::from_secs(2),
    );

    let found =
        most_recent_in_config_dir(root.path(), &cwd, now, Duration::from_secs(600)).unwrap();
    assert_eq!(found.native_id, winner.to_string());
    assert_eq!(found.source, ForeignSessionSource::ClaudeCode);
}

#[test]
fn recent_probe_bounds_content_reads() {
    let root = TempDir::new().unwrap();
    let cwd = root.path().join("repo");
    fs::create_dir_all(&cwd).unwrap();
    let project = scoped_project(root.path(), &cwd);
    let now = UNIX_EPOCH + Duration::from_secs(3_100_000);
    for i in 0..MAX_RECENT_CONTENT_READS {
        write_session(
            &project,
            uuid::Uuid::from_u128(100 + i as u128),
            &[user(Path::new("/other"), "wrong cwd")],
            now - Duration::from_secs(i as u64),
        );
    }
    write_session(
        &project,
        uuid::Uuid::from_u128(999),
        &[user(&cwd, "outside read budget")],
        now - Duration::from_secs(MAX_RECENT_CONTENT_READS as u64),
    );

    assert_eq!(
        most_recent_in_config_dir(root.path(), &cwd, now, Duration::from_secs(600)),
        RecentProbe::Incomplete,
    );
}

#[test]
fn recent_probe_fails_closed_at_directory_entry_cap() {
    let root = TempDir::new().unwrap();
    let cwd = root.path().join("repo");
    fs::create_dir_all(&cwd).unwrap();
    let project = scoped_project(root.path(), &cwd);
    let now = UNIX_EPOCH + Duration::from_secs(3_200_000);
    write_session(
        &project,
        uuid::Uuid::from_u128(1_500),
        &[user(&cwd, "must not win after incomplete discovery")],
        now,
    );
    for index in 0..MAX_RECENT_DIRECTORY_ENTRIES {
        fs::write(project.join(format!("junk-{index:03}")), "").unwrap();
    }

    assert_eq!(
        most_recent_in_config_dir(root.path(), &cwd, now, Duration::from_secs(600)),
        RecentProbe::Incomplete,
    );
}

#[test]
fn filters_bad_rows_and_uses_newest_duplicate_with_title_precedence() {
    let root = TempDir::new().unwrap();
    let cwd = root.path().join("repo");
    fs::create_dir_all(&cwd).unwrap();
    let now = UNIX_EPOCH + Duration::from_secs(4_000_000);
    let projects = root.path().join("projects");
    let duplicate = uuid::Uuid::from_u128(1);

    write_session(
        &projects.join("old"),
        duplicate,
        &[
            user(&cwd, "first prompt"),
            json!({"type": "custom-title", "customTitle": "old title"}),
        ],
        now - Duration::from_secs(20),
    );
    write_session(
        &projects.join("new"),
        duplicate,
        &[
            user(&cwd, "first prompt"),
            json!({"summary": "summary"}),
            json!({"lastPrompt": "last prompt"}),
            json!({"aiTitle": "ai title"}),
            json!({"customTitle": "custom title", "gitBranch": "feature"}),
        ],
        now - Duration::from_secs(10),
    );
    write_session(
        &projects.join("unreadable-newest"),
        duplicate,
        &[user(Path::new("/other"), "newest wrong cwd")],
        now,
    );
    write_session(
        &projects.join("sidechain"),
        uuid::Uuid::from_u128(2),
        &[json!({
            "type": "user",
            "cwd": cwd.display().to_string(),
            "isSidechain": true,
            "message": {"content": "hidden"}
        })],
        now,
    );
    write_session(
        &projects.join("no-title"),
        uuid::Uuid::from_u128(3),
        &[json!({"type": "system", "cwd": cwd.display().to_string()})],
        now,
    );
    write_session(
        &projects.join("wrong-cwd"),
        uuid::Uuid::from_u128(4),
        &[user(Path::new("/other"), "wrong")],
        now,
    );
    write_session(
        &projects.join("stale"),
        uuid::Uuid::from_u128(5),
        &[user(&cwd, "stale")],
        now - MAX_SESSION_AGE - Duration::from_secs(1),
    );
    let zero_dir = projects.join("zero");
    fs::create_dir_all(&zero_dir).unwrap();
    fs::write(
        zero_dir.join(format!("{}.jsonl", uuid::Uuid::from_u128(6))),
        "",
    )
    .unwrap();
    fs::create_dir_all(projects.join("nested").join("subagents")).unwrap();
    fs::write(
        projects
            .join("nested")
            .join("subagents")
            .join(format!("{}.jsonl", uuid::Uuid::from_u128(7))),
        user(&cwd, "nested").to_string(),
    )
    .unwrap();

    let project_dirs = fs::read_dir(&projects)
        .unwrap()
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .map(|entry| entry.path())
        .collect::<Vec<_>>();
    let approved_root = ApprovedRoot::new(root.path()).unwrap();
    let sessions = scan_project_dirs(&approved_root, &project_dirs, &cwd, now);
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].native_id, duplicate.to_string());
    assert_eq!(sessions[0].title, "custom title");
    assert_eq!(sessions[0].branch.as_deref(), Some("feature"));
}

#[test]
fn bounds_results_and_orders_ties_by_id() {
    let root = TempDir::new().unwrap();
    let cwd = root.path().join("repo");
    fs::create_dir_all(&cwd).unwrap();
    let now = UNIX_EPOCH + Duration::from_secs(5_000_000);
    let project = scoped_project(root.path(), &cwd);
    for i in 1..=55_u128 {
        write_session(
            &project,
            uuid::Uuid::from_u128(i),
            &[user(&cwd, &format!("prompt {i}"))],
            now - Duration::from_secs((i / 2) as u64),
        );
    }

    let sessions = scan_in_config_dir(root.path(), &cwd, now);
    assert_eq!(sessions.len(), MAX_SESSIONS_PER_TOOL);
    assert!(
        sessions
            .windows(2)
            .all(|pair| pair[0].updated_at > pair[1].updated_at
                || (pair[0].updated_at == pair[1].updated_at
                    && pair[0].native_id < pair[1].native_id))
    );
}

#[test]
fn skips_meta_and_tool_noise_for_first_prompt() {
    let root = TempDir::new().unwrap();
    let cwd = root.path().join("repo");
    fs::create_dir_all(&cwd).unwrap();
    let now = UNIX_EPOCH + Duration::from_secs(6_000_000);
    write_session(
        &scoped_project(root.path(), &cwd),
        uuid::Uuid::from_u128(100),
        &[
            json!({
                "type": "user",
                "cwd": cwd.display().to_string(),
                "isMeta": true,
                "message": {"content": "meta"}
            }),
            json!({
                "type": "user",
                "message": {"content": [{"type": "tool_result", "content": "noise"}]}
            }),
            json!({
                "type": "user",
                "message": {"content": "<command-name>review</command-name>"}
            }),
            json!({
                "type": "user",
                "message": {"content": "real prompt"}
            }),
        ],
        now,
    );

    let sessions = scan_in_config_dir(root.path(), &cwd, now);
    assert_eq!(sessions[0].title, "real prompt");
}

#[test]
fn scoped_scan_keeps_newest_over_budget_and_rejects_symlinks() {
    let root = TempDir::new().unwrap();
    let cwd = root.path().join("repo");
    fs::create_dir_all(&cwd).unwrap();
    let now = UNIX_EPOCH + Duration::from_secs(7_000_000);
    let project = scoped_project(root.path(), &cwd);
    for i in 0..520_u128 {
        write_session(
            &project,
            uuid::Uuid::from_u128(1_000 + i),
            &[user(Path::new("/other"), "wrong cwd")],
            now - Duration::from_secs(i as u64 + 1),
        );
    }
    let valid = uuid::Uuid::from_u128(2_000);
    write_session(
        &project,
        valid,
        &[user(&cwd, "valid after invalid rows")],
        now,
    );
    write_session(
        &project,
        uuid::Uuid::from_u128(2_003),
        &[user(&cwd, "arbitrary future mtime")],
        now + Duration::from_secs(24 * 60 * 60),
    );
    write_session(
        &root.path().join("projects").join("unrelated"),
        uuid::Uuid::from_u128(2_001),
        &[user(&cwd, "must not scan unrelated project")],
        now,
    );

    #[cfg(unix)]
    {
        let outside = root.path().join("outside.jsonl");
        fs::write(&outside, user(&cwd, "symlink escape").to_string()).unwrap();
        std::os::unix::fs::symlink(
            outside,
            project.join(format!("{}.jsonl", uuid::Uuid::from_u128(2_002))),
        )
        .unwrap();
    }

    let sessions = scan_in_config_dir(root.path(), &cwd, now);
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].native_id, valid.to_string());
    assert_eq!(sessions[0].title, "valid after invalid rows");

    let long = PathBuf::from(format!(
        "/{}",
        "a".repeat(projects::MAX_SANITIZED_LENGTH + 1)
    ));
    assert_eq!(projects::project_dir_for_path(root.path(), &long), None);
}

#[cfg(unix)]
#[test]
fn rejects_project_parent_symlink_escape() {
    let config = TempDir::new().unwrap();
    let outside = TempDir::new().unwrap();
    let cwd = config.path().join("repo");
    fs::create_dir_all(&cwd).unwrap();
    let now = UNIX_EPOCH + Duration::from_secs(7_500_000);
    let project = scoped_project(outside.path(), &cwd);
    write_session(
        &project,
        uuid::Uuid::from_u128(2_100),
        &[user(&cwd, "outside parent")],
        now,
    );
    std::os::unix::fs::symlink(
        outside.path().join("projects"),
        config.path().join("projects"),
    )
    .unwrap();

    assert!(scan_in_config_dir(config.path(), &cwd, now).is_empty());
}

#[test]
fn linked_worktree_scope_includes_main_and_siblings() {
    let root = TempDir::new().unwrap();
    let main = root.path().join("main");
    fs::create_dir_all(&main).unwrap();
    let repository = git2::Repository::init(&main).unwrap();
    let signature = git2::Signature::now("test", "test@example.com").unwrap();
    let tree = {
        let mut index = repository.index().unwrap();
        let oid = index.write_tree().unwrap();
        repository.find_tree(oid).unwrap()
    };
    repository
        .commit(Some("HEAD"), &signature, &signature, "init", &tree, &[])
        .unwrap();
    drop(tree);
    let linked = root.path().join("linked");
    let sibling = root.path().join("sibling");
    repository.worktree("linked", &linked, None).unwrap();
    repository.worktree("sibling", &sibling, None).unwrap();
    let main_project = scoped_project(root.path(), &dunce::canonicalize(&main).unwrap());
    let linked_project = scoped_project(root.path(), &dunce::canonicalize(&linked).unwrap());
    let sibling_project = scoped_project(root.path(), &dunce::canonicalize(&sibling).unwrap());

    let project_dirs =
        projects::scoped_project_dirs(root.path(), &dunce::canonicalize(&linked).unwrap());
    assert!(project_dirs.contains(&main_project));
    assert!(project_dirs.contains(&linked_project));
    assert!(project_dirs.contains(&sibling_project));
}
