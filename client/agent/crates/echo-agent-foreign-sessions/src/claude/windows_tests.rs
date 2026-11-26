use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use filetime::FileTime;
use serde_json::json;
use tempfile::TempDir;

use super::*;

struct Fixture {
    _root: TempDir,
    config: PathBuf,
    cwd: PathBuf,
    project: PathBuf,
}

fn fixture() -> Fixture {
    let root = TempDir::new().unwrap();
    let config = root.path().join("config");
    let cwd = root.path().join("repo");
    fs::create_dir_all(&config).unwrap();
    fs::create_dir_all(&cwd).unwrap();
    let config = dunce::canonicalize(config).unwrap();
    let cwd = dunce::canonicalize(cwd).unwrap();
    let project = projects::project_dir_path(&config, &cwd).unwrap();
    Fixture {
        _root: root,
        config,
        cwd,
        project,
    }
}

fn write_session(project: &Path, cwd: &Path, id: uuid::Uuid, modified: SystemTime) {
    fs::create_dir_all(project).unwrap();
    let path = project.join(format!("{id}.jsonl"));
    fs::write(
        &path,
        format!(
            "{}\n",
            json!({
                "type": "user",
                "cwd": cwd,
                "message": {"role": "user", "content": "recent work"},
            })
        ),
    )
    .unwrap();
    filetime::set_file_mtime(&path, FileTime::from_system_time(modified)).unwrap();
}

#[test]
fn recent_probe_finds_valid_session() {
    let fixture = fixture();
    let now = SystemTime::now();
    let id = uuid::Uuid::from_u128(1);
    write_session(&fixture.project, &fixture.cwd, id, now);

    assert_eq!(
        most_recent_in_config_dir(&fixture.config, &fixture.cwd, now, Duration::from_secs(600),)
            .unwrap()
            .native_id,
        id.to_string(),
    );
}

#[test]
fn recent_probe_missing_project_is_complete_empty() {
    let fixture = fixture();
    assert_eq!(
        most_recent_in_config_dir(
            &fixture.config,
            &fixture.cwd,
            SystemTime::now(),
            Duration::from_secs(600),
        ),
        RecentProbe::Complete(None),
    );
}

#[test]
fn recent_probe_directory_limit_is_incomplete() {
    let fixture = fixture();
    let now = SystemTime::now();
    write_session(
        &fixture.project,
        &fixture.cwd,
        uuid::Uuid::from_u128(2),
        now,
    );
    for index in 0..MAX_RECENT_DIRECTORY_ENTRIES {
        fs::write(fixture.project.join(format!("junk-{index:03}")), "").unwrap();
    }

    assert_eq!(
        most_recent_in_config_dir(&fixture.config, &fixture.cwd, now, Duration::from_secs(600),),
        RecentProbe::Incomplete,
    );
}

#[test]
fn recent_probe_rejects_reparse_project_directory_when_supported() {
    let fixture = fixture();
    let outside = fixture._root.path().join("outside-project");
    fs::create_dir_all(&outside).unwrap();
    fs::create_dir_all(fixture.project.parent().unwrap()).unwrap();
    if std::os::windows::fs::symlink_dir(&outside, &fixture.project).is_err() {
        return;
    }

    assert_eq!(
        most_recent_in_config_dir(
            &fixture.config,
            &fixture.cwd,
            SystemTime::now(),
            Duration::from_secs(600),
        ),
        RecentProbe::Incomplete,
    );
}
