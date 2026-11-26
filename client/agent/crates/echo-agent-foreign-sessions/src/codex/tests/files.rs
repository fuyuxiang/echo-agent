use super::*;

fn rollout_records(id: uuid::Uuid, cwd: &Path, source: serde_json::Value, title: &str) -> String {
    [
        json!({
            "type": "session_meta",
            "payload": {
                "id": id,
                "cwd": cwd.display().to_string(),
                "source": source
            }
        }),
        json!({
            "type": "event_msg",
            "payload": {"type": "user_message", "message": title}
        }),
    ]
    .into_iter()
    .map(|record| record.to_string())
    .collect::<Vec<_>>()
    .join("\n")
}

#[test]
fn recent_fallback_skips_excluded_sources_and_wrong_cwds() {
    let root = TempDir::new().unwrap();
    let cwd = root.path().join("repo");
    fs::create_dir_all(&cwd).unwrap();
    let now = UNIX_EPOCH + Duration::from_secs(1_800_090_000);
    let day = session_day(root.path(), now);
    fs::create_dir_all(&day).unwrap();
    let excluded = uuid::Uuid::from_u128(10);
    let custom = uuid::Uuid::from_u128(13);
    let wrong_cwd = uuid::Uuid::from_u128(11);
    let winner = uuid::Uuid::from_u128(12);
    for (id, stored_cwd, source, age) in [
        (
            excluded,
            cwd.as_path(),
            json!({"subagent":"review"}),
            Duration::ZERO,
        ),
        (
            custom,
            cwd.as_path(),
            json!({"custom":"atlas"}),
            Duration::from_millis(500),
        ),
        (
            wrong_cwd,
            Path::new("/other"),
            json!("cli"),
            Duration::from_secs(1),
        ),
        (
            winner,
            cwd.as_path(),
            json!("vscode"),
            Duration::from_secs(2),
        ),
    ] {
        let path = day.join(format!("rollout-2027-01-15T12-00-00-{id}.jsonl"));
        fs::write(&path, rollout_records(id, stored_cwd, source, "")).unwrap();
        touch(&path, now - age);
    }

    let found = most_recent_in_home(root.path(), &cwd, now, Duration::from_secs(600)).unwrap();
    assert_eq!(found.native_id, winner.to_string());
    assert_eq!(found.source, ForeignSessionSource::CodexVsCode);
}

#[test]
fn recent_fallback_fails_closed_at_directory_entry_cap() {
    let root = TempDir::new().unwrap();
    let cwd = root.path().join("repo");
    fs::create_dir_all(&cwd).unwrap();
    let now = UNIX_EPOCH + Duration::from_secs(1_800_095_000);
    let day = session_day(root.path(), now);
    fs::create_dir_all(&day).unwrap();
    write_recent_rollout(
        root.path(),
        &cwd,
        now,
        uuid::Uuid::from_u128(15),
        json!("cli"),
    );
    for index in 0..super::super::files::MAX_RECENT_DIRECTORY_ENTRIES {
        fs::write(day.join(format!("junk-{index:03}")), "").unwrap();
    }

    assert_eq!(
        most_recent_in_home(root.path(), &cwd, now, Duration::from_secs(600)),
        RecentProbe::Incomplete,
    );
}

#[test]
fn recent_fallback_includes_old_creation_directory_with_fresh_mtime() {
    let root = TempDir::new().unwrap();
    let cwd = root.path().join("repo");
    fs::create_dir_all(&cwd).unwrap();
    let now = UNIX_EPOCH + Duration::from_secs(1_800_097_000);
    let created_at = now - Duration::from_secs(10 * 24 * 60 * 60);
    let day = session_day(root.path(), created_at);
    fs::create_dir_all(&day).unwrap();
    let id = uuid::Uuid::from_u128(16);
    let path = rollout_path(&day, id);
    fs::write(&path, rollout_records(id, &cwd, json!("cli"), "")).unwrap();
    touch(&path, now);

    assert_eq!(
        most_recent_in_home(root.path(), &cwd, now, Duration::from_secs(600))
            .unwrap()
            .native_id,
        id.to_string(),
    );
}

#[test]
fn falls_back_to_bounded_plain_and_compressed_heads() {
    let root = TempDir::new().unwrap();
    let cwd = root.path().join("repo");
    fs::create_dir_all(&cwd).unwrap();
    let now = UNIX_EPOCH + Duration::from_secs(1_800_100_000);
    let day = session_day(root.path(), now);
    let valid_day = session_day(root.path(), now - Duration::from_secs(24 * 60 * 60));
    fs::create_dir_all(&day).unwrap();
    fs::create_dir_all(&valid_day).unwrap();

    let plain_id = uuid::Uuid::from_u128(20);
    let plain = valid_day.join(format!("rollout-1970-05-08T12-00-00-{plain_id}.jsonl"));
    fs::write(
        &plain,
        format!(
            "{{malformed\n{}",
            rollout_records(plain_id, &cwd, json!("cli"), "filesystem title")
        ),
    )
    .unwrap();
    touch(&plain, now);

    let compressed_id = uuid::Uuid::from_u128(21);
    let compressed = valid_day.join(format!(
        "rollout-1970-05-08T12-00-01-{compressed_id}.jsonl.zst"
    ));
    let compressed_head = rollout_records(compressed_id, &cwd, json!("vscode"), "compressed title");
    fs::write(
        &compressed,
        zstd::encode_all(compressed_head.as_bytes(), 1).unwrap(),
    )
    .unwrap();
    touch(&compressed, now - Duration::from_secs(1));

    let malformed_id = uuid::Uuid::from_u128(22);
    let malformed = valid_day.join(format!(
        "rollout-1970-05-08T12-00-02-{malformed_id}.jsonl.zst"
    ));
    fs::write(&malformed, b"not a zstd frame").unwrap();
    touch(&malformed, now - Duration::from_secs(2));

    let concatenated_id = uuid::Uuid::from_u128(23);
    let concatenated_path = valid_day.join(format!(
        "rollout-1970-05-08T12-00-03-{concatenated_id}.jsonl.zst"
    ));
    let mut concatenated = zstd::encode_all(
        json!({"type":"event_msg","payload":{"type":"user_message","message":"frame one"}})
            .to_string()
            .as_bytes(),
        1,
    )
    .unwrap();
    concatenated.extend(
        zstd::encode_all(
            rollout_records(
                concatenated_id,
                &cwd,
                json!("cli"),
                "must not read frame two",
            )
            .as_bytes(),
            1,
        )
        .unwrap(),
    );
    fs::write(&concatenated_path, concatenated).unwrap();
    touch(&concatenated_path, now - Duration::from_secs(3));

    let skippable_id = uuid::Uuid::from_u128(24);
    let skippable_path = valid_day.join(format!(
        "rollout-1970-05-08T12-00-04-{skippable_id}.jsonl.zst"
    ));
    let payload_len = super::super::files::MAX_COMPRESSED_HEAD_BYTES + 1024;
    let mut skippable = Vec::with_capacity(payload_len + 8);
    skippable.extend(0x184D_2A50_u32.to_le_bytes());
    skippable.extend(u32::try_from(payload_len).unwrap().to_le_bytes());
    skippable.resize(8 + payload_len, 0);
    skippable.extend(
        zstd::encode_all(
            rollout_records(skippable_id, &cwd, json!("cli"), "beyond compressed cap").as_bytes(),
            1,
        )
        .unwrap(),
    );
    fs::write(&skippable_path, &skippable).unwrap();
    assert!(skippable.len() > super::super::files::MAX_COMPRESSED_HEAD_BYTES);
    touch(&skippable_path, now - Duration::from_secs(4));

    let window_id = uuid::Uuid::from_u128(25);
    let window_path = valid_day.join(format!("rollout-1970-05-08T12-00-05-{window_id}.jsonl.zst"));
    let window_head = rollout_records(window_id, &cwd, json!("cli"), "oversized window");
    let mut encoder = zstd::Encoder::new(Vec::new(), 1).unwrap();
    encoder
        .window_log(super::super::files::MAX_ZSTD_WINDOW_LOG + 1)
        .unwrap();
    encoder.include_contentsize(false).unwrap();
    encoder.write_all(window_head.as_bytes()).unwrap();
    let window_frame = encoder.finish().unwrap();
    assert_eq!(
        zstd::decode_all(window_frame.as_slice()).unwrap(),
        window_head.as_bytes()
    );
    let mut limited = zstd::Decoder::new(window_frame.as_slice()).unwrap();
    limited
        .window_log_max(super::super::files::MAX_ZSTD_WINDOW_LOG)
        .unwrap();
    let mut limited_output = Vec::new();
    assert!(limited.read_to_end(&mut limited_output).is_err());
    fs::write(&window_path, window_frame).unwrap();
    touch(&window_path, now - Duration::from_secs(5));

    let output_id = uuid::Uuid::from_u128(26);
    let output_path = valid_day.join(format!("rollout-1970-05-08T12-00-06-{output_id}.jsonl.zst"));
    let output_head = format!(
        "{}\n{}",
        "x".repeat(super::super::files::MAX_HEAD_BYTES + 1),
        rollout_records(output_id, &cwd, json!("cli"), "beyond output cap")
    );
    let output_frame = zstd::encode_all(output_head.as_bytes(), 1).unwrap();
    assert!(
        String::from_utf8(zstd::decode_all(output_frame.as_slice()).unwrap())
            .unwrap()
            .contains(&output_id.to_string())
    );
    fs::write(&output_path, output_frame).unwrap();
    touch(&output_path, now - Duration::from_secs(6));

    for i in 0..520_u128 {
        let id = uuid::Uuid::from_u128(100 + i);
        let path = day.join(format!("rollout-1970-05-08T12-01-00-{id}.jsonl"));
        fs::write(
            &path,
            rollout_records(id, &cwd, json!({"subagent":"review"}), "excluded source"),
        )
        .unwrap();
        touch(&path, now - Duration::from_secs(i as u64 + 1));
    }

    let sessions = scan_in_home(root.path(), &cwd, now);
    assert_eq!(sessions.len(), 2);
    assert_eq!(sessions[0].native_id, plain_id.to_string());
    assert_eq!(sessions[0].title, "filesystem title");
    assert_eq!(sessions[1].native_id, compressed_id.to_string());
    assert_eq!(sessions[1].title, "compressed title");
}

#[test]
fn fallback_requires_complete_matching_first_session_meta() {
    let root = TempDir::new().unwrap();
    let cwd = root.path().join("repo");
    fs::create_dir_all(&cwd).unwrap();
    let now = UNIX_EPOCH + Duration::from_secs(1_800_150_000);
    let day = session_day(root.path(), now);
    fs::create_dir_all(&day).unwrap();
    for (id, first_payload) in [
        (
            uuid::Uuid::from_u128(30),
            json!({"id":uuid::Uuid::from_u128(30),"source":"cli"}),
        ),
        (
            uuid::Uuid::from_u128(31),
            json!({"id":uuid::Uuid::from_u128(31),"cwd":cwd.display().to_string()}),
        ),
        (
            uuid::Uuid::from_u128(32),
            json!({"cwd":cwd.display().to_string(),"source":"cli"}),
        ),
        (
            uuid::Uuid::from_u128(33),
            json!({"id":uuid::Uuid::from_u128(999),"cwd":cwd.display().to_string(),"source":"cli"}),
        ),
    ] {
        let path = day.join(format!("rollout-2027-01-15T12-00-00-{id}.jsonl"));
        fs::write(
            &path,
            [
                json!({"type":"session_meta","payload":first_payload}).to_string(),
                rollout_records(id, &cwd, json!("cli"), "fork copy"),
            ]
            .join("\n"),
        )
        .unwrap();
        touch(&path, now);
    }
    assert!(scan_in_home(root.path(), &cwd, now).is_empty());
}

#[test]
fn rollout_paths_must_remain_under_approved_roots() {
    let root = TempDir::new().unwrap();
    let sessions = root.path().join("sessions");
    fs::create_dir_all(&sessions).unwrap();
    let approved_id = uuid::Uuid::from_u128(600);
    let compressed_id = uuid::Uuid::from_u128(601);
    let approved = rollout_path(&sessions, approved_id);
    let compressed_plain = rollout_path(&sessions, compressed_id);
    let compressed = PathBuf::from(format!("{}.zst", compressed_plain.display()));
    let outside = rollout_path(root.path(), uuid::Uuid::from_u128(602));
    let wrong_extension = sessions.join("not-a-rollout.txt");
    let directory = rollout_path(&sessions, uuid::Uuid::from_u128(603));
    fs::write(&approved, "").unwrap();
    fs::write(&compressed, "").unwrap();
    fs::write(&outside, "").unwrap();
    fs::write(&wrong_extension, "").unwrap();
    fs::create_dir_all(&directory).unwrap();
    let approved_root = ApprovedRoot::new(root.path()).unwrap();
    assert_eq!(
        existing_rollout_path(
            &approved_root,
            &approved.display().to_string(),
            &approved_id.to_string()
        ),
        Some(dunce::canonicalize(&approved).unwrap())
    );
    assert_eq!(
        existing_rollout_path(
            &approved_root,
            &compressed_plain.display().to_string(),
            &compressed_id.to_string()
        ),
        Some(dunce::canonicalize(&compressed).unwrap())
    );
    assert_eq!(
        existing_rollout_path(
            &approved_root,
            &outside.display().to_string(),
            &uuid::Uuid::from_u128(602).to_string()
        ),
        None
    );
    assert_eq!(
        existing_rollout_path(
            &approved_root,
            &wrong_extension.display().to_string(),
            &uuid::Uuid::from_u128(604).to_string()
        ),
        None
    );
    assert_eq!(
        existing_rollout_path(
            &approved_root,
            &directory.display().to_string(),
            &uuid::Uuid::from_u128(603).to_string()
        ),
        None
    );
    let traversal = format!(
        "sessions/../{}",
        outside.file_name().unwrap().to_string_lossy()
    );
    assert_eq!(
        existing_rollout_path(
            &approved_root,
            &traversal,
            &uuid::Uuid::from_u128(602).to_string()
        ),
        None
    );
    let adversarial = sessions.join(format!(
        "rollout-2027-01-15T12-00-00-extra-{}.jsonl",
        uuid::Uuid::from_u128(604)
    ));
    fs::write(&adversarial, "").unwrap();
    assert_eq!(rollout_id(&adversarial), None);
    #[cfg(unix)]
    {
        let link = rollout_path(&sessions, uuid::Uuid::from_u128(602));
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        assert_eq!(
            existing_rollout_path(
                &approved_root,
                &link.display().to_string(),
                &uuid::Uuid::from_u128(602).to_string()
            ),
            None
        );
    }
}

#[cfg(unix)]
#[test]
fn fallback_rejects_sessions_parent_symlink_escape() {
    let root = TempDir::new().unwrap();
    let outside = TempDir::new().unwrap();
    let codex_home = root.path().join("codex");
    let cwd = codex_home.join("repo");
    fs::create_dir_all(&cwd).unwrap();
    let now = UNIX_EPOCH + Duration::from_secs(1_800_300_000);
    let outside_day = session_day(outside.path(), now);
    fs::create_dir_all(&outside_day).unwrap();
    let id = uuid::Uuid::from_u128(5_000);
    let rollout = rollout_path(&outside_day, id);
    fs::write(
        &rollout,
        rollout_records(id, &cwd, json!("cli"), "outside sessions root"),
    )
    .unwrap();
    touch(&rollout, now);
    std::os::unix::fs::symlink(outside.path().join("sessions"), codex_home.join("sessions"))
        .unwrap();
    assert!(scan_in_home(&codex_home, &cwd, now).is_empty());
}

#[test]
fn date_dirs_include_utc_and_offset_boundary_days() {
    let root = Path::new("/sessions");
    let now = system_time_from_millis(
        chrono::DateTime::parse_from_rfc3339("2026-01-01T00:30:00Z")
            .unwrap()
            .timestamp_millis(),
    )
    .unwrap();
    let negative = super::super::files::recent_date_dirs(root, now, -2 * 60 * 60);
    assert_eq!(negative.len(), 32);
    assert!(negative.contains(&root.join("2026/01/01")));
    assert!(negative.contains(&root.join("2025/12/31")));
    let late = system_time_from_millis(
        chrono::DateTime::parse_from_rfc3339("2026-01-01T23:30:00Z")
            .unwrap()
            .timestamp_millis(),
    )
    .unwrap();
    let positive = super::super::files::recent_date_dirs(root, late, 2 * 60 * 60);
    assert_eq!(positive[0], root.join("2026/01/02"));
    assert!(positive.contains(&root.join("2026/01/01")));
}
