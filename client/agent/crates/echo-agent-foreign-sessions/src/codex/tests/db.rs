use super::*;

#[test]
fn codex_query_uses_cwd_index_and_length_metadata_opcode() {
    let root = TempDir::new().unwrap();
    let db_path = root.path().join("state.sqlite");
    create_db(&db_path, &[]);
    let connection = Connection::open(&db_path).unwrap();
    let columns = [
        "id",
        "rollout_path",
        "updated_at_ms",
        "source",
        "cwd",
        "title",
        "first_user_message",
        "archived",
        "git_branch",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<std::collections::HashSet<_>>();
    let sql = super::super::db::scan_sql(&columns).unwrap();
    let mut plan = connection
        .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
        .unwrap();
    let details = plan
        .query_map(params!["/repo", 0_i64, i64::MAX], |row| {
            row.get::<_, String>(3)
        })
        .unwrap()
        .flatten()
        .collect::<Vec<_>>();
    assert!(details.iter().any(|detail| {
        detail.contains("SEARCH threads USING INDEX threads_archived_cwd_updated")
            && detail.contains("archived=? AND cwd=?")
    }));
    assert!(
        details
            .iter()
            .any(|detail| detail.contains("USE TEMP B-TREE FOR ORDER BY"))
    );

    let mut bytecode = connection.prepare(&format!("EXPLAIN {sql}")).unwrap();
    let columns = bytecode
        .query_map(params!["/repo", 0_i64, i64::MAX], |row| {
            Ok((
                row.get::<_, String>(1)?,
                row.get::<_, i64>(3)?,
                row.get::<_, i64>(6)?,
            ))
        })
        .unwrap()
        .flatten()
        .collect::<Vec<_>>();
    for column in [0_i64, 1, 5, 6, 8] {
        assert!(columns.iter().any(|(opcode, p2, p5)| {
            opcode == "Column" && *p2 == column && (*p5 & 0xc0) == 0xc0
        }));
    }
}

#[test]
fn recent_database_probe_is_windowed_bounded_and_keeps_source_filters() {
    let root = TempDir::new().unwrap();
    let cwd = root.path().join("repo");
    let rollout_dir = root.path().join("sessions/2027/01/15");
    fs::create_dir_all(&cwd).unwrap();
    fs::create_dir_all(&rollout_dir).unwrap();
    let now = UNIX_EPOCH + Duration::from_secs(1_800_000_000);
    let winner = uuid::Uuid::from_u128(90);
    let winner_rollout = rollout_path(&rollout_dir, winner);
    fs::write(&winner_rollout, "").unwrap();
    let missing = rollout_path(&rollout_dir, uuid::Uuid::from_u128(91));
    let db_path = root.path().join("state_9.sqlite");
    create_db(
        &db_path,
        &[
            DbRow {
                id: uuid::Uuid::from_u128(93),
                rollout_path: &winner_rollout,
                updated_at_ms: millis_from_system_time(now + Duration::from_secs(1)).unwrap(),
                source: r#"{"custom":"atlas"}"#,
                cwd: &cwd,
                title: "excluded custom source",
                first_user_message: "",
                archived: false,
            },
            DbRow {
                id: uuid::Uuid::from_u128(92),
                rollout_path: &winner_rollout,
                updated_at_ms: millis_from_system_time(now).unwrap(),
                source: r#"{"subagent":"review"}"#,
                cwd: &cwd,
                title: "excluded",
                first_user_message: "",
                archived: false,
            },
            DbRow {
                id: uuid::Uuid::from_u128(91),
                rollout_path: &missing,
                updated_at_ms: millis_from_system_time(now - Duration::from_secs(1)).unwrap(),
                source: "cli",
                cwd: &cwd,
                title: "missing rollout",
                first_user_message: "",
                archived: false,
            },
            DbRow {
                id: winner,
                rollout_path: &winner_rollout,
                updated_at_ms: millis_from_system_time(now - Duration::from_secs(2)).unwrap(),
                source: "vscode",
                cwd: &cwd,
                title: "",
                first_user_message: "",
                archived: false,
            },
        ],
    );

    let found = most_recent_in_home(root.path(), &cwd, now, Duration::from_secs(600)).unwrap();
    assert_eq!(found.native_id, winner.to_string());
    assert_eq!(found.source, ForeignSessionSource::CodexVsCode);

    let columns = [
        "id",
        "rollout_path",
        "updated_at_ms",
        "source",
        "cwd",
        "title",
        "first_user_message",
        "archived",
        "git_branch",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<std::collections::HashSet<_>>();
    let sql = super::super::db::recent_scan_sql(&columns).unwrap();
    assert!(sql.contains("source IN ('cli', 'vscode')"));
    assert!(!sql.contains("custom"));
    assert!(sql.contains(&format!(
        "LIMIT {}",
        super::super::db::MAX_RECENT_DB_CANDIDATES + 1
    )));
}

#[test]
fn recent_database_sentinel_marks_invalid_window_incomplete() {
    let root = TempDir::new().unwrap();
    let cwd = root.path().join("repo");
    let sessions = root.path().join("sessions");
    fs::create_dir_all(&cwd).unwrap();
    fs::create_dir_all(&sessions).unwrap();
    let now = UNIX_EPOCH + Duration::from_secs(1_800_010_000);
    let valid_id = uuid::Uuid::from_u128(190);
    let valid_rollout = rollout_path(&sessions, valid_id);
    fs::write(&valid_rollout, "").unwrap();
    let db_path = root.path().join("state_10.sqlite");
    create_db(&db_path, &[]);
    let connection = Connection::open(&db_path).unwrap();
    for index in 0..super::super::db::MAX_RECENT_DB_CANDIDATES {
        connection
            .execute(
                "INSERT INTO threads VALUES (?1, ?2, ?3, 'cli', ?4, 'invalid', '', 0, NULL)",
                params![
                    uuid::Uuid::from_u128(10_000 + index as u128).to_string(),
                    sessions
                        .join(format!("missing-{index}.jsonl"))
                        .display()
                        .to_string(),
                    millis_from_system_time(now - Duration::from_secs(index as u64)).unwrap(),
                    cwd.display().to_string(),
                ],
            )
            .unwrap();
    }
    assert_eq!(
        most_recent_in_home(root.path(), &cwd, now, Duration::from_secs(600)),
        RecentProbe::Complete(None),
    );
    connection
        .execute(
            "INSERT INTO threads VALUES (?1, ?2, ?3, 'cli', ?4, 'ninth valid', '', 0, NULL)",
            params![
                valid_id.to_string(),
                valid_rollout.display().to_string(),
                millis_from_system_time(now - Duration::from_secs(20)).unwrap(),
                cwd.display().to_string(),
            ],
        )
        .unwrap();
    drop(connection);

    assert_eq!(
        most_recent_in_home(root.path(), &cwd, now, Duration::from_secs(600)),
        RecentProbe::Incomplete,
    );
}

#[test]
fn recent_database_row_decode_error_is_incomplete() {
    let root = TempDir::new().unwrap();
    let cwd = root.path().join("repo");
    let sessions = root.path().join("sessions");
    fs::create_dir_all(&cwd).unwrap();
    fs::create_dir_all(&sessions).unwrap();
    let now = UNIX_EPOCH + Duration::from_secs(1_800_015_000);
    let older_id = uuid::Uuid::from_u128(191);
    let older_rollout = rollout_path(&sessions, older_id);
    fs::write(&older_rollout, "").unwrap();
    let db_path = root.path().join("state_10.sqlite");
    create_db(
        &db_path,
        &[DbRow {
            id: older_id,
            rollout_path: &older_rollout,
            updated_at_ms: millis_from_system_time(now - Duration::from_secs(1)).unwrap(),
            source: "cli",
            cwd: &cwd,
            title: "older valid",
            first_user_message: "",
            archived: false,
        }],
    );
    let broken_id = uuid::Uuid::from_u128(192);
    let broken_rollout = rollout_path(&sessions, broken_id);
    fs::write(&broken_rollout, "").unwrap();
    let connection = Connection::open(&db_path).unwrap();
    connection
        .execute(
            "INSERT INTO threads VALUES (?1, ?2, ?3, 'cli', ?4, CAST(x'80' AS TEXT), '', 0, NULL)",
            params![
                broken_id.to_string(),
                broken_rollout.display().to_string(),
                millis_from_system_time(now).unwrap(),
                cwd.display().to_string(),
            ],
        )
        .unwrap();
    drop(connection);

    assert_eq!(
        most_recent_in_home(root.path(), &cwd, now, Duration::from_secs(600)),
        RecentProbe::Incomplete,
    );
    let full = scan_in_home(root.path(), &cwd, now);
    assert_eq!(full.len(), 1);
    assert_eq!(full[0].native_id, older_id.to_string());
}

#[test]
fn recent_database_tri_state_uses_only_current_generation() {
    let now = UNIX_EPOCH + Duration::from_secs(1_800_020_000);

    let readable = TempDir::new().unwrap();
    let readable_cwd = readable.path().join("repo");
    fs::create_dir_all(&readable_cwd).unwrap();
    let older_id = uuid::Uuid::from_u128(200);
    let older_rollout =
        write_recent_rollout(readable.path(), &readable_cwd, now, older_id, json!("cli"));
    create_db(
        &readable.path().join("state_9.sqlite"),
        &[DbRow {
            id: older_id,
            rollout_path: &older_rollout,
            updated_at_ms: millis_from_system_time(now).unwrap(),
            source: "cli",
            cwd: &readable_cwd,
            title: "obsolete generation",
            first_user_message: "",
            archived: false,
        }],
    );
    create_db(&readable.path().join("state_10.sqlite"), &[]);
    assert!(
        most_recent_in_home(
            readable.path(),
            &readable_cwd,
            now,
            Duration::from_secs(600)
        )
        .is_none(),
        "a usable empty current index must suppress obsolete DB and rollout fallback"
    );

    let unreadable = TempDir::new().unwrap();
    let unreadable_cwd = unreadable.path().join("repo");
    fs::create_dir_all(&unreadable_cwd).unwrap();
    let fallback_id = uuid::Uuid::from_u128(201);
    write_recent_rollout(
        unreadable.path(),
        &unreadable_cwd,
        now,
        fallback_id,
        json!("vscode"),
    );
    fs::write(unreadable.path().join("state_10.sqlite"), "not sqlite").unwrap();
    assert_eq!(
        most_recent_in_home(
            unreadable.path(),
            &unreadable_cwd,
            now,
            Duration::from_secs(600)
        )
        .unwrap()
        .native_id,
        fallback_id.to_string()
    );

    let absent = TempDir::new().unwrap();
    let absent_cwd = absent.path().join("repo");
    fs::create_dir_all(&absent_cwd).unwrap();
    let absent_id = uuid::Uuid::from_u128(202);
    write_recent_rollout(absent.path(), &absent_cwd, now, absent_id, json!("cli"));
    assert_eq!(
        most_recent_in_home(absent.path(), &absent_cwd, now, Duration::from_secs(600))
            .unwrap()
            .native_id,
        absent_id.to_string()
    );
}

#[cfg(unix)]
#[test]
fn recent_database_unsafe_highest_uses_fallback_not_older_database() {
    let root = TempDir::new().unwrap();
    let cwd = root.path().join("repo");
    fs::create_dir_all(&cwd).unwrap();
    let now = UNIX_EPOCH + Duration::from_secs(1_800_025_000);
    let older_id = uuid::Uuid::from_u128(210);
    let older_rollout = write_recent_rollout(root.path(), &cwd, now, older_id, json!("cli"));
    touch(&older_rollout, now - Duration::from_secs(60 * 60));
    let older_db = root.path().join("state_9.sqlite");
    create_db(
        &older_db,
        &[DbRow {
            id: older_id,
            rollout_path: &older_rollout,
            updated_at_ms: millis_from_system_time(now).unwrap(),
            source: "cli",
            cwd: &cwd,
            title: "must not use older DB",
            first_user_message: "",
            archived: false,
        }],
    );
    std::os::unix::fs::symlink(&older_db, root.path().join("state_10.sqlite")).unwrap();

    let fallback_id = uuid::Uuid::from_u128(211);
    write_recent_rollout(root.path(), &cwd, now, fallback_id, json!("vscode"));
    assert_eq!(
        most_recent_in_home(root.path(), &cwd, now, Duration::from_secs(600))
            .unwrap()
            .native_id,
        fallback_id.to_string(),
    );
}

#[test]
fn highest_database_filters_sources_paths_cwd_and_millis() {
    let root = TempDir::new().unwrap();
    let cwd = root.path().join("repo");
    fs::create_dir_all(&cwd).unwrap();
    let now = UNIX_EPOCH + Duration::from_secs(1_800_000_000);
    let updated = now - Duration::from_secs(2);
    let updated_ms = millis_from_system_time(updated).unwrap();
    let rollout_dir = root.path().join("sessions/2026/01/01");
    fs::create_dir_all(&rollout_dir).unwrap();
    let old_rollout = rollout_path(&rollout_dir, uuid::Uuid::from_u128(1));
    fs::write(&old_rollout, "").unwrap();
    create_db(
        &root.path().join("state_5.sqlite"),
        &[DbRow {
            id: uuid::Uuid::from_u128(1),
            rollout_path: &old_rollout,
            updated_at_ms: updated_ms,
            source: "cli",
            cwd: &cwd,
            title: "old generation",
            first_user_message: "",
            archived: false,
        }],
    );

    let rollout = rollout_path(&rollout_dir, uuid::Uuid::from_u128(2));
    fs::write(&rollout, "").unwrap();
    let compressed_plain = rollout_path(&rollout_dir, uuid::Uuid::from_u128(3));
    fs::write(format!("{}.zst", compressed_plain.display()), "").unwrap();
    let chatgpt_rollout = rollout_path(&rollout_dir, uuid::Uuid::from_u128(9));
    fs::write(&chatgpt_rollout, "").unwrap();
    let missing = rollout_path(&rollout_dir, uuid::Uuid::from_u128(5));
    let stale_ms = millis_from_system_time(
        now - super::super::super::MAX_SESSION_AGE - Duration::from_secs(1),
    )
    .unwrap();
    create_db(
        &root.path().join("state_9.sqlite"),
        &[
            DbRow {
                id: uuid::Uuid::from_u128(2),
                rollout_path: &rollout,
                updated_at_ms: updated_ms,
                source: "vscode",
                cwd: &cwd,
                title: "",
                first_user_message: "fallback title",
                archived: false,
            },
            DbRow {
                id: uuid::Uuid::from_u128(3),
                rollout_path: &compressed_plain,
                updated_at_ms: updated_ms - 1,
                source: r#"{"custom":"atlas"}"#,
                cwd: &cwd,
                title: "compressed",
                first_user_message: "",
                archived: false,
            },
            DbRow {
                id: uuid::Uuid::from_u128(9),
                rollout_path: &chatgpt_rollout,
                updated_at_ms: updated_ms - 2,
                source: r#"{"custom":"chatgpt"}"#,
                cwd: &cwd,
                title: "chatgpt",
                first_user_message: "",
                archived: false,
            },
            DbRow {
                id: uuid::Uuid::from_u128(4),
                rollout_path: &rollout,
                updated_at_ms: updated_ms,
                source: r#"{"subagent":"review"}"#,
                cwd: &cwd,
                title: "subagent",
                first_user_message: "",
                archived: false,
            },
            DbRow {
                id: uuid::Uuid::from_u128(5),
                rollout_path: &missing,
                updated_at_ms: updated_ms,
                source: "cli",
                cwd: &cwd,
                title: "missing",
                first_user_message: "",
                archived: false,
            },
            DbRow {
                id: uuid::Uuid::from_u128(6),
                rollout_path: &rollout,
                updated_at_ms: stale_ms,
                source: "cli",
                cwd: &cwd,
                title: "stale",
                first_user_message: "",
                archived: false,
            },
            DbRow {
                id: uuid::Uuid::from_u128(7),
                rollout_path: &rollout,
                updated_at_ms: updated_ms,
                source: "cli",
                cwd: Path::new("/other"),
                title: "wrong cwd",
                first_user_message: "",
                archived: false,
            },
            DbRow {
                id: uuid::Uuid::from_u128(8),
                rollout_path: &rollout,
                updated_at_ms: updated_ms,
                source: "cli",
                cwd: &cwd,
                title: "archived",
                first_user_message: "",
                archived: true,
            },
        ],
    );

    let sessions = scan_in_home(root.path(), &cwd, now);
    assert_eq!(
        sessions
            .iter()
            .map(|session| session.title.as_str())
            .collect::<Vec<_>>(),
        vec!["fallback title", "compressed", "chatgpt"]
    );
    assert_eq!(sessions[0].updated_at, updated);
    assert_eq!(sessions[0].source, ForeignSessionSource::CodexVsCode);
    assert_eq!(sessions[1].source, ForeignSessionSource::CodexAtlas);
    assert_eq!(sessions[2].source, ForeignSessionSource::CodexChatGpt);
}

#[test]
fn empty_newer_database_uses_older_nonempty_generation() {
    let root = TempDir::new().unwrap();
    let cwd = root.path().join("repo");
    let rollout_dir = root.path().join("sessions/2027/01/15");
    fs::create_dir_all(&cwd).unwrap();
    fs::create_dir_all(&rollout_dir).unwrap();
    let now = UNIX_EPOCH + Duration::from_secs(1_800_050_000);
    let id = uuid::Uuid::from_u128(40);
    let rollout = rollout_path(&rollout_dir, id);
    fs::write(&rollout, "").unwrap();
    create_db(
        &root.path().join("state_9.sqlite"),
        &[DbRow {
            id,
            rollout_path: &rollout,
            updated_at_ms: millis_from_system_time(now).unwrap(),
            source: "cli",
            cwd: &cwd,
            title: "older nonempty generation",
            first_user_message: "",
            archived: false,
        }],
    );
    create_db(&root.path().join("state_10.sqlite"), &[]);

    let sessions = scan_in_home(root.path(), &cwd, now);

    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].native_id, id.to_string());
    assert_eq!(sessions[0].title, "older nonempty generation");
}

#[test]
fn empty_databases_fall_back_to_rollout_files() {
    let root = TempDir::new().unwrap();
    let cwd = root.path().join("repo");
    let now = UNIX_EPOCH + Duration::from_secs(1_800_100_000);
    let day = session_day(root.path(), now);
    fs::create_dir_all(&cwd).unwrap();
    fs::create_dir_all(&day).unwrap();
    let id = uuid::Uuid::from_u128(41);
    let rollout = rollout_path(&day, id);
    let contents = [
        json!({
            "type": "session_meta",
            "payload": {
                "id": id,
                "cwd": cwd.display().to_string(),
                "source": "cli"
            }
        }),
        json!({
            "type": "event_msg",
            "payload": {
                "type": "user_message",
                "message": "rollout fallback"
            }
        }),
    ]
    .into_iter()
    .map(|record| record.to_string())
    .collect::<Vec<_>>()
    .join("\n");
    fs::write(&rollout, contents).unwrap();
    touch(&rollout, now);
    create_db(&root.path().join("state_10.sqlite"), &[]);

    let sessions = scan_in_home(root.path(), &cwd, now);

    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].native_id, id.to_string());
    assert_eq!(sessions[0].title, "rollout fallback");
}

#[test]
fn database_window_qualifies_past_invalid_rows() {
    let root = TempDir::new().unwrap();
    let cwd = root.path().join("repo");
    let sessions = root.path().join("sessions");
    fs::create_dir_all(&cwd).unwrap();
    fs::create_dir_all(&sessions).unwrap();
    let now = UNIX_EPOCH + Duration::from_secs(1_800_200_000);
    let valid_id = uuid::Uuid::from_u128(500);
    let valid_path = rollout_path(&sessions, valid_id);
    fs::write(&valid_path, "").unwrap();
    let db_path = root.path().join("state_10.sqlite");
    create_db(
        &db_path,
        &[DbRow {
            id: valid_id,
            rollout_path: &valid_path,
            updated_at_ms: millis_from_system_time(now - Duration::from_secs(100)).unwrap(),
            source: "cli",
            cwd: &cwd,
            title: "valid after window",
            first_user_message: "",
            archived: false,
        }],
    );
    let connection = Connection::open(&db_path).unwrap();
    for i in 0..60_u128 {
        connection
            .execute(
                "INSERT INTO threads VALUES (?1, ?2, ?3, 'cli', ?4, 'invalid', '', 0, NULL)",
                params![
                    uuid::Uuid::from_u128(1_000 + i).to_string(),
                    sessions
                        .join(format!("missing-{i}.jsonl"))
                        .display()
                        .to_string(),
                    millis_from_system_time(now - Duration::from_secs(i as u64)).unwrap(),
                    cwd.display().to_string(),
                ],
            )
            .unwrap();
    }
    for i in 0..220_u128 {
        connection
            .execute(
                "INSERT INTO threads VALUES (?1, ?2, ?3, ?4, ?5, 'subagent', '', 0, NULL)",
                params![
                    uuid::Uuid::from_u128(2_000 + i).to_string(),
                    valid_path.display().to_string(),
                    millis_from_system_time(now - Duration::from_secs(i as u64)).unwrap(),
                    r#"{"subagent":"review"}"#,
                    cwd.display().to_string(),
                ],
            )
            .unwrap();
    }
    drop(connection);
    let found = scan_in_home(root.path(), &cwd, now);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].title, "valid after window");
}

#[test]
fn database_window_normalizes_units_and_filters_future_rows_before_limit() {
    let root = TempDir::new().unwrap();
    let cwd = root.path().join("repo");
    let sessions = root.path().join("sessions");
    fs::create_dir_all(&cwd).unwrap();
    fs::create_dir_all(&sessions).unwrap();
    let now = UNIX_EPOCH + Duration::from_secs(1_800_250_000);
    let valid_id = uuid::Uuid::from_u128(2_500);
    let valid_path = rollout_path(&sessions, valid_id);
    fs::write(&valid_path, "").unwrap();
    let db_path = root.path().join("state_10.sqlite");
    create_db(
        &db_path,
        &[DbRow {
            id: valid_id,
            rollout_path: &valid_path,
            updated_at_ms: i64::try_from(
                (now - Duration::from_secs(1))
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_secs(),
            )
            .unwrap(),
            source: "cli",
            cwd: &cwd,
            title: "newer seconds row",
            first_user_message: "",
            archived: false,
        }],
    );
    let connection = Connection::open(&db_path).unwrap();
    for (prefix, offset) in [
        ("stale", -(10 * 24 * 60 * 60_i64)),
        ("future", 24 * 60 * 60),
    ] {
        for i in 0..205_u128 {
            let timestamp = if offset < 0 {
                now - Duration::from_secs((-offset) as u64 + i as u64)
            } else {
                now + Duration::from_secs(offset as u64 + i as u64)
            };
            connection
                .execute(
                    "INSERT INTO threads VALUES (?1, ?2, ?3, 'cli', ?4, ?5, '', 0, NULL)",
                    params![
                        uuid::Uuid::from_u128(3_000 + i + if offset > 0 { 1_000 } else { 0 })
                            .to_string(),
                        sessions
                            .join(format!("missing-{prefix}-{i}.jsonl"))
                            .display()
                            .to_string(),
                        millis_from_system_time(timestamp).unwrap(),
                        cwd.display().to_string(),
                        prefix,
                    ],
                )
                .unwrap();
        }
    }
    drop(connection);
    let found = scan_in_home(root.path(), &cwd, now);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].title, "newer seconds row");
}

#[test]
fn each_required_sql_predicate_protects_the_candidate_window() {
    for case in 0..5_u128 {
        let root = TempDir::new().unwrap();
        let cwd = root.path().join("repo");
        let sessions = root.path().join("sessions");
        fs::create_dir_all(&cwd).unwrap();
        fs::create_dir_all(&sessions).unwrap();
        let now = UNIX_EPOCH + Duration::from_secs(1_800_260_000 + case as u64);
        let valid_id = uuid::Uuid::from_u128(6_000 + case);
        let valid_path = rollout_path(&sessions, valid_id);
        fs::write(&valid_path, "").unwrap();
        let db_path = root.path().join("state_10.sqlite");
        create_db(
            &db_path,
            &[DbRow {
                id: valid_id,
                rollout_path: &valid_path,
                updated_at_ms: millis_from_system_time(now - Duration::from_secs(1)).unwrap(),
                source: "cli",
                cwd: &cwd,
                title: "valid required row",
                first_user_message: "",
                archived: false,
            }],
        );
        let connection = Connection::open(&db_path).unwrap();
        for i in 0..201_u128 {
            let id_text = uuid::Uuid::from_u128(7_000 + i).to_string();
            let mut id = SqlValue::Text(id_text.clone());
            let rollout_text = sessions
                .join(format!("missing-required-{case}-{i}.jsonl"))
                .display()
                .to_string();
            let mut rollout = SqlValue::Text(rollout_text.clone());
            let mut updated = SqlValue::Integer(millis_from_system_time(now).unwrap());
            let source = SqlValue::Text("cli".to_owned());
            let stored_cwd = SqlValue::Text(cwd.display().to_string());
            match case {
                0 => id = SqlValue::Blob(id_text.into_bytes()),
                1 => id = SqlValue::Text(oversized_text(64, i % 2 == 1)),
                2 => rollout = SqlValue::Blob(rollout_text.into_bytes()),
                3 => rollout = SqlValue::Text(oversized_text(16 * 1024, i % 2 == 1)),
                _ => updated = SqlValue::Real(millis_from_system_time(now).unwrap() as f64 + 0.5),
            }
            connection
                .execute(
                    "INSERT INTO threads VALUES (?1, ?2, ?3, ?4, ?5, 'invalid', '', 0, NULL)",
                    params![id, rollout, updated, source, stored_cwd],
                )
                .unwrap();
        }
        drop(connection);
        let found = scan_in_home(root.path(), &cwd, now);
        assert_eq!(found.len(), 1, "required predicate case {case}");
        assert_eq!(found[0].native_id, valid_id.to_string());
    }
}

#[test]
fn optional_codex_metadata_degrades_without_dropping_rows() {
    let root = TempDir::new().unwrap();
    let cwd = root.path().join("repo");
    let sessions = root.path().join("sessions");
    fs::create_dir_all(&cwd).unwrap();
    fs::create_dir_all(&sessions).unwrap();
    let now = UNIX_EPOCH + Duration::from_secs(1_800_270_000);
    let db_path = root.path().join("state_10.sqlite");
    create_db(&db_path, &[]);
    let connection = Connection::open(&db_path).unwrap();
    let expected = [
        "fallback oversized title",
        "fallback wrong title type",
        "title with oversized fallback",
        "title with wrong fallback type",
        "title with oversized branch",
        "title with wrong branch type",
    ];
    for (index, expected_title) in expected.iter().enumerate() {
        let id = uuid::Uuid::from_u128(8_000 + index as u128);
        let path = rollout_path(&sessions, id);
        fs::write(&path, "").unwrap();
        let mut title = SqlValue::Text((*expected_title).to_owned());
        let mut first = SqlValue::Text((*expected_title).to_owned());
        let mut branch = SqlValue::Text("main".to_owned());
        match index {
            0 => title = SqlValue::Text(oversized_text(64 * 1024, false)),
            1 => title = SqlValue::Blob(b"wrong title".to_vec()),
            2 => first = SqlValue::Text(oversized_text(64 * 1024, true)),
            3 => first = SqlValue::Blob(b"wrong fallback".to_vec()),
            4 => branch = SqlValue::Text(oversized_text(4 * 1024, false)),
            _ => branch = SqlValue::Blob(b"wrong branch".to_vec()),
        }
        connection
            .execute(
                "INSERT INTO threads VALUES (?1, ?2, ?3, 'cli', ?4, ?5, ?6, 0, ?7)",
                params![
                    id.to_string(),
                    path.display().to_string(),
                    millis_from_system_time(now - Duration::from_secs(index as u64)).unwrap(),
                    cwd.display().to_string(),
                    title,
                    first,
                    branch,
                ],
            )
            .unwrap();
    }
    drop(connection);
    let found = scan_in_home(root.path(), &cwd, now);
    assert_eq!(found.len(), expected.len());
    for (index, expected_title) in expected.iter().enumerate() {
        let id = uuid::Uuid::from_u128(8_000 + index as u128).to_string();
        let session = found
            .iter()
            .find(|session| session.native_id == id)
            .unwrap();
        assert_eq!(session.title, *expected_title);
        if index >= 4 {
            assert_eq!(session.branch, None);
        }
    }
}

#[test]
fn state_database_probes_have_a_supported_generation_ceiling() {
    let root = TempDir::new().unwrap();
    for i in 0..100 {
        fs::write(root.path().join(format!("unrelated-{i:03}")), "").unwrap();
    }
    fs::write(root.path().join("state_2.sqlite"), "").unwrap();
    let boundary = root
        .path()
        .join(format!("state_{MAX_STATE_DB_GENERATION}.sqlite"));
    let beyond = root
        .path()
        .join(format!("state_{}.sqlite", MAX_STATE_DB_GENERATION + 1));
    fs::write(&boundary, "").unwrap();
    fs::write(&beyond, "").unwrap();
    let approved_root = ApprovedRoot::new(root.path()).unwrap();
    let root_path = approved_root.path();
    assert_eq!(
        state_databases(&approved_root).collect::<Vec<_>>(),
        vec![
            root_path.join(format!("state_{MAX_STATE_DB_GENERATION}.sqlite")),
            root_path.join("state_2.sqlite"),
        ]
    );
    fs::remove_file(root_path.join(format!("state_{MAX_STATE_DB_GENERATION}.sqlite"))).unwrap();
    assert_eq!(
        state_databases(&approved_root).collect::<Vec<_>>(),
        vec![root_path.join("state_2.sqlite")]
    );
}

#[cfg(unix)]
#[test]
fn state_database_probe_rejects_symlink_escape() {
    let root = TempDir::new().unwrap();
    let codex_home = root.path().join("codex");
    fs::create_dir_all(&codex_home).unwrap();
    let outside = root.path().join("outside.sqlite");
    fs::write(&outside, "").unwrap();
    std::os::unix::fs::symlink(
        &outside,
        codex_home.join(format!("state_{MAX_STATE_DB_GENERATION}.sqlite")),
    )
    .unwrap();
    let approved_root = ApprovedRoot::new(&codex_home).unwrap();
    assert!(state_databases(&approved_root).next().is_none());
}

#[test]
fn normalizes_legacy_seconds_and_millis_then_uses_shared_recency() {
    let now = UNIX_EPOCH + Duration::from_secs(1_800_000_000);
    let expected = now - Duration::from_secs(10);
    let seconds = 1_799_999_990_i64;
    let millis = millis_from_system_time(expected).unwrap();
    assert_eq!(
        super::super::db::normalize_updated_at(seconds),
        Some(expected)
    );
    assert_eq!(
        super::super::db::normalize_updated_at(millis),
        Some(expected)
    );
    let future = now + Duration::from_secs(24 * 60 * 60);
    assert_eq!(
        super::super::db::normalize_updated_at(millis_from_system_time(future).unwrap()),
        Some(future)
    );
    assert!(!super::super::super::is_within(
        future,
        now,
        super::super::super::MAX_SESSION_AGE,
    ));
}
