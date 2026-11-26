use pretty_assertions::assert_eq;

use super::*;
use crate::test_util::make_worktree_record as make_record;

fn render_report(report: &DiskUsageReport, now: i64) -> String {
    let mut out = Vec::new();
    display::print_report(report, now, &mut out).unwrap();
    String::from_utf8(out).unwrap()
}

fn measured(path: &Path) -> Option<u64> {
    physical_dir_size(path, Volume::of(path)).measure.bytes()
}

fn modified(path: &Path) -> Option<i64> {
    physical_dir_size(path, Volume::of(path))
        .measure
        .last_modified()
}

fn untracked_row(bytes: u64) -> WorktreeUsage {
    WorktreeUsage {
        bytes: Some(bytes),
        kind: WorktreeKind::Session,
        registration: Registration::Untracked,
        last_modified_at: None,
        path: "/wt-home/worktrees/echoagent/wt-1".into(),
    }
}

fn tracked_row(bytes: u64, rec: TrackedRow) -> WorktreeUsage {
    WorktreeUsage {
        registration: Registration::Tracked(rec),
        ..untracked_row(bytes)
    }
}

fn record(id: &str, created_at: i64) -> TrackedRow {
    TrackedRow {
        id: id.into(),
        status: WorktreeStatus::Alive,
        created_at,
        last_accessed_at: None,
        label: None,
        repo_name: "repo".into(),
        git_ref: None,
    }
}

fn worktrees_report(worktrees: Vec<WorktreeUsage>, total_bytes: u64) -> DiskUsageReport {
    DiskUsageReport {
        echo_agent_home: "/wt-home".into(),
        total_bytes,
        top_level_dirs: vec![DirUsage {
            name: WORKTREES_DIR.to_owned(),
            bytes: Some(total_bytes),
        }],
        registry: RegistryState::Read,
        worktrees,
        ..DiskUsageReport::default()
    }
}

#[test]
fn collect_report_joins_registry_and_flags_untracked() {
    let tmp = tempfile::TempDir::new().unwrap();
    let base = dunce::canonicalize(tmp.path()).unwrap();
    let home = base.join("echo-agent-home");
    let tracked = home.join("worktrees/echoagent/wt-tracked");
    let untracked = home.join("worktrees/echoagent/wt-untracked");
    let external = base.join("external-repo");
    std::fs::create_dir_all(&tracked).unwrap();
    std::fs::create_dir_all(&untracked).unwrap();
    std::fs::create_dir_all(&external).unwrap();
    std::fs::write(tracked.join("big.bin"), vec![b'x'; 65536]).unwrap();
    std::fs::write(untracked.join("small.bin"), vec![b'x'; 4096]).unwrap();
    std::fs::write(external.join("huge.bin"), vec![b'x'; 131_072]).unwrap();
    std::fs::create_dir_all(home.join("sessions")).unwrap();
    std::fs::write(home.join("sessions/log.jsonl"), vec![b'x'; 4096]).unwrap();

    let db = WorktreeDb::open(&home).unwrap();
    db.register(&make_record("wt-tracked", &tracked, "my-feature"))
        .unwrap();
    db.register(&make_record("wt-external", &external, "elsewhere"))
        .unwrap();

    let report = collect_report(&home).unwrap();

    assert_eq!(
        report.top_level_dirs,
        vec![
            DirUsage {
                name: "worktrees".into(),
                bytes: measured(&home.join("worktrees")),
            },
            DirUsage {
                name: "sessions".into(),
                bytes: measured(&home.join("sessions")),
            },
        ],
        "largest dir sorts first"
    );
    assert!(report.root_files_bytes > 0);
    assert_eq!(report.skips.skipped_entries, 0);
    assert_eq!(report.registry, RegistryState::Read);
    assert!(
        Path::new(&report.registry_path).starts_with(&home),
        "registry_path must be resolved under the home: {}",
        report.registry_path
    );
    assert_eq!(report.worktrees_outside_managed_roots, 1);

    assert_eq!(
        report.worktrees,
        vec![
            WorktreeUsage {
                last_modified_at: modified(&tracked),
                path: tracked.to_string_lossy().into_owned(),
                ..tracked_row(
                    measured(&tracked).unwrap(),
                    TrackedRow {
                        label: Some("my-feature".into()),
                        ..record("wt-tracked", 0)
                    },
                )
            },
            WorktreeUsage {
                last_modified_at: modified(&untracked),
                path: untracked.to_string_lossy().into_owned(),
                ..untracked_row(measured(&untracked).unwrap())
            },
        ]
    );
}

#[cfg(unix)]
#[test]
fn record_registered_via_symlinked_home_joins_as_one_row() {
    let tmp = tempfile::TempDir::new().unwrap();
    let base = dunce::canonicalize(tmp.path()).unwrap();
    let real_home = base.join("real-home");
    let wt = real_home.join("worktrees/echoagent/wt-a");
    std::fs::create_dir_all(&wt).unwrap();
    std::fs::write(wt.join("f.bin"), vec![b'x'; 4096]).unwrap();
    let link_home = base.join("link-home");
    std::os::unix::fs::symlink(&real_home, &link_home).unwrap();

    let db = WorktreeDb::open(&real_home).unwrap();
    db.register(&make_record(
        "wt-a",
        &link_home.join("worktrees/echoagent/wt-a"),
        "via-link",
    ))
    .unwrap();

    let report = collect_report(&real_home).unwrap();
    assert_eq!(
        report.worktrees.len(),
        1,
        "a record stored under a symlinked home must not also appear as untracked"
    );
    assert!(report.worktrees[0].is_tracked());
    assert_eq!(report.worktrees[0].label(), "via-link");
}

#[cfg(unix)]
#[test]
fn duplicate_discovered_dirs_size_once() {
    let tmp = tempfile::TempDir::new().unwrap();
    let home = dunce::canonicalize(tmp.path())
        .unwrap()
        .join("echo-agent-home");
    let wt = home.join("worktrees/echoagent/wt-a");
    std::fs::create_dir_all(&wt).unwrap();
    std::fs::write(wt.join("f.bin"), vec![b'x'; 4096]).unwrap();
    std::os::unix::fs::symlink(&wt, home.join("worktrees/echoagent/wt-alias")).unwrap();

    let report = collect_report(&home).unwrap();
    assert_eq!(
        report.worktrees.len(),
        1,
        "two discovered entries canonicalizing to one dir must yield one row"
    );
    assert!(!report.worktrees[0].is_tracked());
    assert_eq!(report.worktrees[0].bytes, measured(&wt));
}

#[cfg(unix)]
#[test]
fn escape_symlink_is_counted_not_sized() {
    let tmp = tempfile::TempDir::new().unwrap();
    let base = dunce::canonicalize(tmp.path()).unwrap();
    let home = base.join("echo-agent-home");
    std::fs::create_dir_all(home.join("worktrees/echoagent")).unwrap();
    let external = base.join("external");
    std::fs::create_dir_all(&external).unwrap();
    std::fs::write(external.join("huge.bin"), vec![b'x'; 65536]).unwrap();
    std::os::unix::fs::symlink(&external, home.join("worktrees/echoagent/escape")).unwrap();

    let report = collect_report(&home).unwrap();
    assert!(
        report.worktrees.is_empty(),
        "an escape symlink's external target must not be sized as a row"
    );
    assert_eq!(report.worktrees_outside_managed_roots, 1);
}

#[cfg(unix)]
#[test]
fn top_level_symlink_costs_its_own_inode_not_the_target() {
    let tmp = tempfile::TempDir::new().unwrap();
    let base = dunce::canonicalize(tmp.path()).unwrap();
    let home = base.join("echo-agent-home");
    std::fs::create_dir_all(&home).unwrap();
    let external = base.join("external-huge.bin");
    std::fs::write(&external, vec![b'x'; 1 << 20]).unwrap();
    let link = home.join("huge.bin");
    std::os::unix::fs::symlink(&external, &link).unwrap();

    let report = collect_report(&home).unwrap();

    let link_bytes = physical_file_size(&std::fs::symlink_metadata(&link).unwrap());
    let target_bytes = physical_file_size(&std::fs::metadata(&external).unwrap());
    assert!(
        link_bytes < target_bytes,
        "the target must be big enough for following it to be visible: \
         link {link_bytes}, target {target_bytes}"
    );
    assert_eq!(
        report.root_files_bytes, link_bytes,
        "a top-level symlink must cost exactly its own inode, never the target"
    );
    assert_eq!(
        report.total_bytes, link_bytes,
        "an external target must not inflate the home total"
    );
    assert_eq!(report.skips.skipped_entries, 0);
}

#[test]
fn record_at_missing_path_is_omitted() {
    let tmp = tempfile::TempDir::new().unwrap();
    let home = dunce::canonicalize(tmp.path())
        .unwrap()
        .join("echo-agent-home");
    std::fs::create_dir_all(&home).unwrap();
    let db = WorktreeDb::open(&home).unwrap();
    db.register(&make_record(
        "wt-gone",
        &home.join("worktrees/echoagent/wt-gone"),
        "gone",
    ))
    .unwrap();

    let report = collect_report(&home).unwrap();
    assert_eq!(report.registry, RegistryState::Read);
    assert!(report.worktrees.is_empty());
    assert_eq!(report.worktrees_outside_managed_roots, 0);
    assert_eq!(report.skips.skipped_entries, 0);
}

#[test]
fn registry_absent_reports_untracked_rows() {
    let tmp = tempfile::TempDir::new().unwrap();
    let home = dunce::canonicalize(tmp.path())
        .unwrap()
        .join("echo-agent-home");
    let wt = home.join("worktrees/echoagent/wt-a");
    std::fs::create_dir_all(&wt).unwrap();
    std::fs::write(wt.join("f.bin"), vec![b'x'; 4096]).unwrap();

    let dir_names = |path: &Path| -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(path)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    };
    let before = dir_names(&home);
    let report = collect_report(&home).unwrap();
    assert_eq!(dir_names(&home), before, "collecting must not create files");
    assert_eq!(report.registry, RegistryState::Absent);
    assert_eq!(report.worktrees.len(), 1);
    assert!(!report.worktrees[0].is_tracked());
}

#[test]
fn corrupt_registry_degrades_to_untracked_rows() {
    let tmp = tempfile::TempDir::new().unwrap();
    let home = dunce::canonicalize(tmp.path())
        .unwrap()
        .join("echo-agent-home");
    let wt = home.join("worktrees/echoagent/wt-a");
    std::fs::create_dir_all(&wt).unwrap();
    std::fs::write(wt.join("f.bin"), vec![b'x'; 4096]).unwrap();
    std::fs::write(
        WorktreeDb::resolve_db_path(&home),
        b"garbage, not an sqlite header",
    )
    .unwrap();

    let report = collect_report(&home).unwrap();
    assert_eq!(report.registry, RegistryState::Corrupt);
    assert!(!report.top_level_dirs.is_empty());
    assert_eq!(report.worktrees.len(), 1);
    assert!(!report.worktrees[0].is_tracked());
    assert_eq!(
        serde_json::to_value(&report).unwrap()["registry"],
        "corrupt"
    );
}

// The fallback must not re-measure an excluded row against its own volume:
// that printed bytes the total did not hold.
#[cfg(unix)]
#[test]
fn a_row_off_the_anchor_reports_no_size() {
    let tmp = tempfile::TempDir::new().unwrap();
    let wt = tmp.path().join("worktrees/echoagent/wt-a");
    std::fs::create_dir_all(&wt).unwrap();
    std::fs::write(wt.join("payload.bin"), vec![b'x'; 65536]).unwrap();
    let elsewhere = Volume::of(&wt).other_device_for_test();
    let mut issues = WalkIssues::default();

    let size = row_size(&wt, &HashMap::new(), &mut issues, elsewhere);

    assert_eq!(size, Measure::Elsewhere);
    assert_eq!(size.bytes(), None, "no row prints bytes no total holds");
    assert_eq!(issues.other_filesystems, 1);
    assert!(
        measured(&wt).is_some_and(|bytes| bytes >= 65536),
        "the bytes are there to measure: the anchor is the only reason the row has none"
    );
}

// A swapped arm shipped silently once. Only `Corrupt` advises deleting.
#[test]
fn every_open_outcome_maps_to_its_state() {
    let tmp = tempfile::TempDir::new().unwrap();
    let home = tmp.path().join("echo-agent-home");
    let path = WorktreeDb::resolve_db_path(&home);
    let db = WorktreeDb::open(&home).unwrap();
    db.register(&make_record(
        "wt-a",
        &home.join("worktrees/echoagent/wt-a"),
        "lbl",
    ))
    .unwrap();

    let opened = classify(WorktreeDb::open_read_only(&home));
    assert_eq!(opened.0, RegistryState::Read);
    assert_eq!(opened.1.len(), 1, "a readable registry yields its records");

    let cases = [
        (
            RegistryOpen::Absent { path: path.clone() },
            RegistryState::Absent,
        ),
        (
            RegistryOpen::Busy {
                path: path.clone(),
                error: anyhow::anyhow!("database busy after 10s"),
            },
            RegistryState::Busy,
        ),
        (
            RegistryOpen::Failed {
                path: path.clone(),
                error: anyhow::anyhow!("permission denied"),
            },
            RegistryState::Unopenable,
        ),
    ];
    for (open, want) in cases {
        let (state, records, reported) = classify(open);
        assert_eq!(state, want);
        assert!(records.is_empty());
        assert_eq!(reported, path, "every state names the file");
    }
}

// Deleting a registry that is merely unopenable loses every label,
// creation time, and session id in it.
#[test]
fn unopenable_registry_is_not_reported_as_corrupt() {
    let tmp = tempfile::TempDir::new().unwrap();
    let home = dunce::canonicalize(tmp.path())
        .unwrap()
        .join("echo-agent-home");
    let wt = home.join("worktrees/echoagent/wt-a");
    std::fs::create_dir_all(&wt).unwrap();
    std::fs::write(wt.join("f.bin"), vec![b'x'; 4096]).unwrap();
    std::fs::create_dir_all(WorktreeDb::resolve_db_path(&home)).unwrap();

    let report = collect_report(&home).unwrap();

    assert_eq!(report.registry, RegistryState::Unopenable);
    let text = render_report(&report, 0);
    assert!(text.contains("could not be opened"), "{text}");
    assert!(
        !text.contains("db rebuild") && !text.contains("damaged"),
        "an unopenable registry must never be proposed for deletion: {text}"
    );
}

// A column showing only creation prints 40d beside a `--max-age 7d` hint
// for a worktree gc will not touch.
#[test]
fn age_column_reads_what_gc_reads() {
    const DAY: i64 = 86_400;
    let now = 100 * DAY;
    let used_yesterday = tracked_row(
        60,
        TrackedRow {
            last_accessed_at: Some(now - DAY),
            ..record("wt-used", now - 40 * DAY)
        },
    );
    let text = render_report(&worktrees_report(vec![used_yesterday], 100), now);
    assert!(text.contains("1d ago"), "{text}");
    assert!(
        !text.contains("40d ago"),
        "the column must not overstate staleness against gc: {text}"
    );
}

// Four hand-copied assignments; swapping two of them kept CI green.
#[test]
fn walk_issues_convert_to_report_counters() {
    let issues = WalkIssues {
        unreadable_dirs: 2,
        unstatable_entries: 3,
        other_filesystems: 4,
    };
    assert_eq!(
        SkipCounts::from(issues),
        SkipCounts {
            skipped_entries: 5,
            unreadable_dirs: 2,
            unstatable_entries: 3,
            other_filesystem_dirs: 4,
        },
        "each counter keeps its name, and skipped_entries is the sum of the two failures"
    );
}

// Flipping Busy to Corrupt puts "damaged, remove it" in front of a user
// whose registry is only busy.
#[test]
fn sqlite_failure_kinds_map_to_states() {
    let cases = [
        (SqliteFailureKind::Busy, RegistryState::Busy),
        (SqliteFailureKind::Corrupt, RegistryState::Corrupt),
        (SqliteFailureKind::Other, RegistryState::Unopenable),
    ];
    for (kind, want) in cases {
        assert_eq!(state_for(kind), want, "{kind:?}");
    }
}

// A read-only WAL open leaves sidecars, so sizing runs first. Only their
// absence from the total proves the ordering.
#[test]
fn the_registry_open_lands_after_sizing() {
    let tmp = tempfile::TempDir::new().unwrap();
    let home = dunce::canonicalize(tmp.path())
        .unwrap()
        .join("echo-agent-home");
    let db_path = WorktreeDb::resolve_db_path(&home);
    {
        let db = WorktreeDb::open(&home).unwrap();
        db.register(&make_record(
            "wt-a",
            &home.join("worktrees/echoagent/wt-a"),
            "lbl",
        ))
        .unwrap();
    }
    let sidecars: Vec<PathBuf> = ["-wal", "-shm"]
        .iter()
        .map(|suffix| PathBuf::from(format!("{}{suffix}", db_path.display())))
        .collect();
    assert!(
        sidecars.iter().all(|p| !p.exists()),
        "a closed connection checkpoints its sidecars away"
    );

    let report = collect_report(&home).unwrap();

    let db_bytes = physical_file_size(&std::fs::symlink_metadata(&db_path).unwrap());
    assert_eq!(
        report.root_files_bytes, db_bytes,
        "the total holds the database and nothing this run's open created"
    );
    assert!(
        sidecars.iter().any(|p| p.exists()),
        "the read-only open does leave a sidecar, which is why sizing goes first"
    );
}

#[test]
fn worktrees_dominate_at_half_of_total() {
    struct Case {
        name: &'static str,
        dir_bytes: u64,
        row_bytes: u64,
        total_bytes: u64,
        dominate: bool,
    }
    let cases = [
        Case {
            name: "half the total",
            dir_bytes: 50,
            row_bytes: 0,
            total_bytes: 100,
            dominate: true,
        },
        Case {
            name: "one byte under half",
            dir_bytes: 49,
            row_bytes: 0,
            total_bytes: 100,
            dominate: false,
        },
        Case {
            name: "an empty home",
            dir_bytes: 0,
            row_bytes: 0,
            total_bytes: 0,
            dominate: false,
        },
        Case {
            name: "rows only",
            dir_bytes: 0,
            row_bytes: 50,
            total_bytes: 100,
            dominate: true,
        },
    ];
    for case in cases {
        let report = DiskUsageReport {
            echo_agent_home: "/home/user/.echo-agent".into(),
            total_bytes: case.total_bytes,
            top_level_dirs: vec![DirUsage {
                name: WORKTREES_DIR.to_owned(),
                bytes: Some(case.dir_bytes),
            }],
            registry: RegistryState::Read,
            worktrees: vec![untracked_row(case.row_bytes)],
            ..DiskUsageReport::default()
        };
        assert_eq!(report.worktrees_dominate(), case.dominate, "{}", case.name);
    }
}

#[test]
fn json_shape_is_frozen() {
    let report = DiskUsageReport {
        schema_version: SCHEMA_VERSION,
        echo_agent_home: "/home/user/.echo-agent".into(),
        total_bytes: 100,
        volume_capacity_bytes: Some(1_000),
        volume_available_bytes: Some(600),
        top_level_dirs: vec![DirUsage {
            name: "worktrees".into(),
            bytes: Some(90),
        }],
        root_files_bytes: 10,
        skips: SkipCounts::default(),
        unfollowed_dir_symlinks: 0,
        worktrees_outside_managed_roots: 0,
        registry: RegistryState::Read,
        registry_path: "/home/user/.echo-agent/worktrees.db".into(),
        worktrees: vec![
            WorktreeUsage {
                last_modified_at: Some(1_700_005_000),
                path: "/home/user/.echo-agent/worktrees/echoagent/wt-1".into(),
                ..tracked_row(
                    90,
                    TrackedRow {
                        last_accessed_at: Some(1_700_009_999),
                        label: Some("my-feature".into()),
                        repo_name: "echoagent".into(),
                        git_ref: Some("brian/fix".into()),
                        ..record("wt-1", 1_700_000_000)
                    },
                )
            },
            WorktreeUsage {
                kind: WorktreeKind::Pool,
                last_modified_at: Some(1_700_002_000),
                path: "/home/user/.echo-agent/worktree_pool/inst/wt-2".into(),
                ..untracked_row(10)
            },
        ],
    };
    assert_eq!(
        serde_json::to_value(&report).unwrap(),
        serde_json::json!({
            "schema_version": 1,
            "echo_agent_home": "/home/user/.echo-agent",
            "total_bytes": 100,
            "volume_capacity_bytes": 1_000,
            "volume_available_bytes": 600,
            "top_level_dirs": [{ "name": "worktrees", "bytes": 90 }],
            "root_files_bytes": 10,
            "skipped_entries": 0,
            "unreadable_dirs": 0,
            "unstatable_entries": 0,
            "other_filesystem_dirs": 0,
            "unfollowed_dir_symlinks": 0,
            "worktrees_outside_managed_roots": 0,
            "registry": "read",
            "registry_path": "/home/user/.echo-agent/worktrees.db",
            "worktrees": [
                {
                    "bytes": 90,
                    "kind": "session",
                    "tracked": true,
                    "id": "wt-1",
                    "status": "alive",
                    "created_at": 1_700_000_000,
                    "last_accessed_at": 1_700_009_999,
                    "last_modified_at": 1_700_005_000,
                    "label": "my-feature",
                    "repo_name": "echoagent",
                    "git_ref": "brian/fix",
                    "path": "/home/user/.echo-agent/worktrees/echoagent/wt-1",
                },
                {
                    "bytes": 10,
                    "kind": "pool",
                    "tracked": false,
                    "id": null,
                    "status": null,
                    "created_at": null,
                    "last_accessed_at": null,
                    "last_modified_at": 1_700_002_000,
                    "label": null,
                    "repo_name": null,
                    "git_ref": null,
                    "path": "/home/user/.echo-agent/worktree_pool/inst/wt-2",
                },
            ],
        })
    );

    // Serialize is hand-written, and `to_value` would not see a reshuffle.
    let pretty = serde_json::to_string_pretty(&report.worktrees[0]).unwrap();
    let keys: Vec<&str> = pretty
        .lines()
        .filter_map(|line| line.trim().strip_prefix('"')?.split('"').next())
        .collect();
    assert_eq!(
        keys,
        [
            "bytes",
            "kind",
            "tracked",
            "id",
            "status",
            "created_at",
            "last_accessed_at",
            "last_modified_at",
            "label",
            "repo_name",
            "git_ref",
            "path",
        ]
    );
}

#[test]
fn missing_home_json_is_valid_and_empty() {
    let mut out = Vec::new();
    write_report(
        &empty_report(Path::new("/nonexistent/.echo-agent")),
        /*json*/ true,
        &mut out,
    )
    .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(json["schema_version"], 1);
    assert_eq!(json["registry"], "absent");
    assert_eq!(json["registry_path"], "");
    assert_eq!(json["total_bytes"], 0);
    assert_eq!(json["worktrees"], serde_json::json!([]));
}

#[test]
fn print_report_truncates_long_labels_and_keeps_columns_aligned() {
    let long_label = "a".repeat(30);
    let report = DiskUsageReport {
        echo_agent_home: "/wt-home".into(),
        total_bytes: 300,
        top_level_dirs: vec![DirUsage {
            name: WORKTREES_DIR.to_owned(),
            bytes: Some(300),
        }],
        registry: RegistryState::Read,
        worktrees: vec![
            WorktreeUsage {
                path: "/wt-home/worktrees/echoagent/wt-long".into(),
                ..tracked_row(
                    150,
                    TrackedRow {
                        label: Some(long_label.clone()),
                        ..record("wt-long", 0)
                    },
                )
            },
            WorktreeUsage {
                path: "/wt-home/worktrees/echoagent/wt-dead".into(),
                ..tracked_row(
                    100,
                    TrackedRow {
                        status: WorktreeStatus::Dead,
                        label: Some("组件更新".into()),
                        ..record("wt-dead", 0)
                    },
                )
            },
            WorktreeUsage {
                path: "/wt-home/worktrees/echoagent/wt-short".into(),
                ..untracked_row(50)
            },
        ],
        ..DiskUsageReport::default()
    };
    let text = render_report(&report, 1_000_000_000);

    assert!(text.contains(&format!("{}…", &long_label[..23])));
    assert!(!text.contains(&long_label));
    assert!(text.contains("session (dead)"));
    assert!(text.contains("untracked (session)"));
    assert!(text.contains("组件更新"));
    crate::test_util::assert_path_column_aligned(&text, "worktrees/echoagent/wt-");
}

#[test]
fn every_skip_counter_renders_singular_and_plural() {
    struct Case {
        set: fn(&mut DiskUsageReport, u64),
        one: &'static str,
        two: &'static str,
        /// A `>= 0` typo would print the sentence on every clean report.
        fragment: &'static str,
    }
    let cases = [
        Case {
            set: |r, n| r.skips.unreadable_dirs = n,
            one: "1 directory could not be read; what is under it may be missing from the total. RUST_LOG=debug names it.",
            two: "2 directories could not be read; what is under them may be missing from the total. RUST_LOG=debug names them.",
            fragment: "may be missing from the total",
        },
        Case {
            set: |r, n| r.skips.unstatable_entries = n,
            one: "1 entry could not be read and is not counted.",
            two: "2 entries could not be read and are not counted.",
            fragment: "could not be read and",
        },
        Case {
            set: |r, n| r.skips.other_filesystem_dirs = n,
            one: "1 directory is on another filesystem and is not counted, here or in any row.",
            two: "2 directories are on another filesystem and are not counted, here or in any row.",
            fragment: "on another filesystem",
        },
        Case {
            set: |r, n| r.unfollowed_dir_symlinks = n,
            one: "1 top-level symlink to a directory is not followed, so its contents are missing from the total.",
            two: "2 top-level symlinks to directories are not followed, so their contents are missing from the total.",
            fragment: "not followed",
        },
        Case {
            set: |r, n| r.worktrees_outside_managed_roots = n,
            one: "1 worktree outside the managed worktree dirs is not shown here.",
            two: "2 worktrees outside the managed worktree dirs are not shown here.",
            fragment: "outside the managed worktree dirs",
        },
    ];
    for case in cases {
        for (n, want) in [(1u64, case.one), (2, case.two)] {
            let mut report = worktrees_report(vec![untracked_row(50)], 100);
            (case.set)(&mut report, n);
            let text = render_report(&report, 0);
            assert!(text.contains(want), "n={n}: want {want:?} in\n{text}");
        }
        let mut zero = worktrees_report(vec![untracked_row(50)], 100);
        (case.set)(&mut zero, 0);
        let text = render_report(&zero, 0);
        assert!(
            !text.contains(case.fragment),
            "n=0 must print nothing, got {:?} in\n{text}",
            case.fragment
        );
    }
}

#[test]
fn print_report_renders_registry_notices() {
    struct Case {
        name: &'static str,
        registry: RegistryState,
        rows: bool,
        expected: &'static [&'static str],
        absent: &'static [&'static str],
    }
    let cases = [
        Case {
            name: "a read registry names no registry state",
            registry: RegistryState::Read,
            rows: true,
            expected: &["1 worktree outside the managed worktree dirs is not shown here."],
            absent: &["Worktree registry", "db rebuild"],
        },
        Case {
            name: "an absent registry says why rows read as untracked",
            registry: RegistryState::Absent,
            rows: true,
            expected: &["Worktree registry not found"],
            absent: &[],
        },
        Case {
            name: "a fresh install has no rows the notice could mislabel",
            registry: RegistryState::Absent,
            rows: false,
            expected: &["No worktrees found."],
            absent: &["Worktree registry"],
        },
        Case {
            name: "a corrupt registry names the file even with no rows",
            registry: RegistryState::Corrupt,
            rows: false,
            expected: &["Worktree registry is damaged", "Remove", "worktrees.db"],
            absent: &[],
        },
        Case {
            name: "a busy registry blames the peer, never the file",
            registry: RegistryState::Busy,
            rows: true,
            expected: &["in use by another process", "Retry in a moment."],
            absent: &[
                "db rebuild",
                "damaged",
                "Remove $ECHO_AGENT_HOME/worktrees.db",
            ],
        },
        Case {
            name: "an unopenable registry names the file without proposing deletion",
            registry: RegistryState::Unopenable,
            rows: true,
            expected: &[
                "could not be opened",
                "worktrees.db",
                "Check its permissions.",
            ],
            absent: &[
                "db rebuild",
                "damaged",
                "Remove $ECHO_AGENT_HOME/worktrees.db",
            ],
        },
    ];
    for case in cases {
        let rows = if case.rows {
            vec![untracked_row(50)]
        } else {
            Vec::new()
        };
        let report = DiskUsageReport {
            worktrees_outside_managed_roots: 1,
            registry: case.registry,
            registry_path: "/wt-home/worktrees.db".into(),
            ..worktrees_report(rows, 100)
        };
        let text = render_report(&report, 0);
        for want in case.expected {
            assert!(
                text.contains(want),
                "{}: want {want:?} in\n{text}",
                case.name
            );
        }
        for unwanted in case.absent {
            assert!(
                !text.contains(unwanted),
                "{}: {unwanted:?} must not print in\n{text}",
                case.name
            );
        }
    }
}

// Bare `gc` reclaims nothing: without `--max-age` the age pass is off, and
// the pass only walks registry records.
#[test]
fn reclaim_hint_names_a_sequence_that_frees_space() {
    const AGE: &str = "run `echoagent worktree gc --max-age 7d --dry-run`";
    const RM: &str = "Remove one with `echoagent worktree rm --dry-run <path>`";
    let tracked = tracked_row(60, record("wt-1", 0));

    let text = render_report(&worktrees_report(vec![tracked], 100), 0);
    assert!(text.contains(AGE), "{text}");
    assert!(!text.contains(RM), "{text}");
    assert!(
        text.contains("keeps a worktree whose work it cannot find elsewhere"),
        "the hint must say what gc will refuse to reclaim: {text}"
    );

    let text = render_report(&worktrees_report(vec![untracked_row(60)], 100), 0);
    assert!(text.contains(RM), "{text}");
    assert!(!text.contains(AGE), "{text}");
}

#[cfg(unix)]
#[test]
fn clone_note_states_the_double_count_only_when_the_volume_proves_it() {
    struct Case {
        name: &'static str,
        volume: Option<(u64, u64)>,
        total_bytes: u64,
        expected: &'static str,
        absent: &'static str,
    }
    const PROVEN: &str =
        "Total exceeds the used space on this volume, so shared blocks are counted once per path.";
    const GENERAL: &str =
        "Worktree clones share storage with their source, so the total can exceed real disk use.";
    let cases = [
        Case {
            name: "1000 capacity less 600 free is 400 in use, under the total",
            volume: Some((1_000, 600)),
            total_bytes: 1_500,
            expected: PROVEN,
            absent: GENERAL,
        },
        Case {
            name: "no volume figures proves nothing",
            volume: None,
            total_bytes: 900,
            expected: GENERAL,
            absent: PROVEN,
        },
        Case {
            name: "a total within the used space proves nothing",
            volume: Some((1_000, 100)),
            total_bytes: 900,
            expected: GENERAL,
            absent: PROVEN,
        },
    ];
    for case in cases {
        let mut report = worktrees_report(vec![untracked_row(900)], case.total_bytes);
        let (capacity, available) = case.volume.unzip();
        report.volume_capacity_bytes = capacity;
        report.volume_available_bytes = available;
        let text = render_report(&report, 0);
        assert!(text.contains(case.expected), "{}: {text}", case.name);
        assert!(!text.contains(case.absent), "{}: {text}", case.name);
    }

    let mut excluded = worktrees_report(vec![untracked_row(900)], 1_500);
    excluded.volume_capacity_bytes = Some(1_000);
    excluded.volume_available_bytes = Some(600);
    excluded.skips.other_filesystem_dirs = 1;
    let text = render_report(&excluded, 0);
    assert!(text.contains(PROVEN), "{text}");
    assert!(
        text.contains(
            "1 directory is on another filesystem and is not counted, here or in any row."
        ),
        "{text}"
    );
}

#[cfg(unix)]
#[test]
fn symlinked_worktrees_dir_is_surfaced_not_silently_dropped() {
    let tmp = tempfile::TempDir::new().unwrap();
    let base = dunce::canonicalize(tmp.path()).unwrap();
    let home = base.join("echo-agent-home");
    std::fs::create_dir_all(&home).unwrap();
    let elsewhere = base.join("worktrees-on-another-disk");
    let wt = elsewhere.join("echoagent/wt-a");
    std::fs::create_dir_all(&wt).unwrap();
    std::fs::write(wt.join(".git"), "gitdir: /repo/.git/worktrees/wt-a\n").unwrap();
    std::fs::write(wt.join("big.bin"), vec![b'x'; 1 << 20]).unwrap();
    std::os::unix::fs::symlink(&elsewhere, home.join(WORKTREES_DIR)).unwrap();

    let report = collect_report(&home).unwrap();

    assert_eq!(report.unfollowed_dir_symlinks, 1);
    assert!(
        report.top_level_dirs.is_empty(),
        "a symlinked worktrees dir is not a dir entry, so it never reaches the breakdown: {:?}",
        report.top_level_dirs
    );
    assert_eq!(report.worktrees.len(), 1);
    assert!(
        report.worktrees[0].bytes.unwrap() > report.total_bytes,
        "the row must outsize a total that never walked the target"
    );
    assert!(
        render_report(&report, 0).contains("1 top-level symlink to a directory is not followed"),
        "the omission must be stated, not left to the arithmetic"
    );
}

#[cfg(unix)]
#[test]
#[serial_test::serial(ECHO_AGENT_HOME)]
// serial keys are independent locks, so a test setting both must hold both.
#[serial_test::serial(HOME)]
fn symlinked_default_home_keeps_home_label() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fake_home = tmp.path().join("home");
    let real_echo_agent = tmp.path().join("echo-agent-on-disk");
    std::fs::create_dir_all(&fake_home).unwrap();
    std::fs::create_dir_all(&real_echo_agent).unwrap();
    std::os::unix::fs::symlink(&real_echo_agent, fake_home.join(".echo-agent")).unwrap();
    let _home = crate::test_util::EnvVarGuard::set("HOME", &fake_home);

    let resolved = dunce::canonicalize(&fake_home).unwrap().join(".echo-agent");
    let canonical = dunce::canonicalize(&resolved).unwrap();
    assert_ne!(canonical, resolved, "the symlink must actually resolve");
    assert_eq!(
        crate::util::display_echo_agent_home_prefix_for(&canonical),
        "~/.echo-agent"
    );
}
