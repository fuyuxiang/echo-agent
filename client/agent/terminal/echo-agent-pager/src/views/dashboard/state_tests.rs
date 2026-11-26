use super::*;

/// `set_error_toast` prefixes the message with the error glyph
/// (`✗`/`x`) so the verbatim-rendering badge marks it as an error.
#[test]
fn set_error_toast_prefixes_error_glyph() {
    let mut state = DashboardState::new();
    state.set_error_toast("boom");
    assert_eq!(
        state.error_toast.as_deref(),
        Some(format!("{} boom", crate::glyphs::ballot_x()).as_str()),
    );
}

#[test]
fn filter_parser_agent_prefix() {
    match parse_filter("a:reviewer") {
        FilterValue::Agent(s) => assert_eq!(s, "reviewer"),
        other => panic!("expected agent filter, got {other:?}"),
    }
}

#[test]
fn filter_parser_empty_agent_clears() {
    assert!(matches!(parse_filter("a:"), FilterValue::None));
    assert!(matches!(parse_filter("a:   "), FilterValue::None));
}

#[test]
fn filter_parser_state_known() {
    assert!(matches!(
        parse_filter("s:needs-input"),
        FilterValue::State(RowState::NeedsInput)
    ));
    assert!(matches!(
        parse_filter("s:needs_input"),
        FilterValue::State(RowState::NeedsInput)
    ));
    assert!(matches!(
        parse_filter("s:needsinput"),
        FilterValue::State(RowState::NeedsInput)
    ));
    assert!(matches!(
        parse_filter("s:working"),
        FilterValue::State(RowState::Working)
    ));
    assert!(matches!(
        parse_filter("s:idle"),
        FilterValue::State(RowState::Idle)
    ));
    assert!(matches!(
        parse_filter("s:blocked"),
        FilterValue::State(RowState::Blocked)
    ));
    assert!(matches!(
        parse_filter("s:completed"),
        FilterValue::State(RowState::Completed)
    ));
    assert!(matches!(
        parse_filter("s:failed"),
        FilterValue::State(RowState::Failed)
    ));
}

#[test]
fn filter_parser_state_unknown_falls_back_to_substring() {
    match parse_filter("s:foobar") {
        FilterValue::Substring(s) => assert_eq!(s, "foobar"),
        other => panic!("expected substring fallback, got {other:?}"),
    }
}

/// `s:` empty now mirrors `a:` empty: returns None,
/// not Substring("").
#[test]
fn filter_parser_state_empty_is_none() {
    match parse_filter("s:") {
        FilterValue::None => {}
        other => panic!("expected None, got {other:?}"),
    }
}

/// `#<n>` keeps the `#` in the substring needle so it
/// never matches arbitrary digits.
#[test]
fn filter_parser_pr_prefix_keeps_hash() {
    match parse_filter("#42") {
        FilterValue::Substring(s) => assert_eq!(s, "#42"),
        other => panic!("expected substring fallback, got {other:?}"),
    }
}

#[test]
fn filter_parser_free_text() {
    match parse_filter("auth flow") {
        FilterValue::Substring(s) => assert_eq!(s, "auth flow"),
        other => panic!("expected substring filter, got {other:?}"),
    }
}

#[test]
fn persisted_row_id_round_trip_top_level() {
    let id = PersistedRowId::TopLevel {
        session_id: "sess-abc".into(),
    };
    let key = id.to_key();
    assert_eq!(key, "top:sess-abc");
    assert_eq!(PersistedRowId::from_key(&key), Some(id));
}

#[test]
fn persisted_row_id_round_trip_subagent() {
    let id = PersistedRowId::Subagent {
        parent_session_id: "p-1".into(),
        child_session_id: "c-1".into(),
    };
    let key = id.to_key();
    assert_eq!(key, "sub:p-1:c-1");
    assert_eq!(PersistedRowId::from_key(&key), Some(id));
}

#[test]
fn persisted_row_id_subagent_with_colon_in_child_id() {
    // The child session id portion may itself contain colons; we
    // only split on the first colon after `sub:<parent>:`.
    let raw = "sub:parent-x:abc:def:ghi";
    let parsed = PersistedRowId::from_key(raw).unwrap();
    match parsed {
        PersistedRowId::Subagent {
            parent_session_id,
            child_session_id,
        } => {
            assert_eq!(parent_session_id, "parent-x");
            assert_eq!(child_session_id, "abc:def:ghi");
        }
        _ => panic!("expected subagent variant"),
    }
}

#[test]
fn persisted_row_id_invalid() {
    assert!(PersistedRowId::from_key("garbage").is_none());
    // top:<empty> rejected.
    assert!(PersistedRowId::from_key("top:").is_none());
    // sub: with no parent rejected.
    assert!(PersistedRowId::from_key("sub::child").is_none());
    // sub:<parent> with no child rejected.
    assert!(PersistedRowId::from_key("sub:parent:").is_none());
    // sub: with no colon between parent/child rejected.
    assert!(PersistedRowId::from_key("sub:foo").is_none());
}

#[test]
fn group_priority_ordering() {
    assert!(RowState::NeedsInput.group_priority() > RowState::Working.group_priority());
    assert!(RowState::Working.group_priority() > RowState::Idle.group_priority());
    assert!(RowState::Idle.group_priority() > RowState::Completed.group_priority());
    assert!(RowState::Completed.group_priority() > RowState::Failed.group_priority());
}

#[test]
fn compact_cwd_strips_home() {
    let p = Path::new("/Users/alice/projects/echoagent");
    assert_eq!(compact_cwd(p, Some("/Users/alice")), "~/projects/echoagent");
}

#[test]
fn compact_cwd_no_home_match() {
    let p = Path::new("/var/tmp/x");
    assert_eq!(compact_cwd(p, Some("/Users/alice")), "/var/tmp/x");
}

/// When `cwd == home`, return bare `"~"` (no
/// trailing slash). This test pins the behaviour.
#[test]
fn compact_cwd_path_equals_home() {
    let p = Path::new("/Users/alice");
    assert_eq!(compact_cwd(p, Some("/Users/alice")), "~");
}

// Vacuous `rename_draft_caps_at_100_chars` deleted —
// covered by the substantive `rename_at_cap_drops_extra_char` and
// `rename_under_cap_appends` tests below, which assert exact
// character at the cap boundary.

/// Edge case 20: stale row ids in `pinned` / `reorder` are dropped
/// on `gc_stale_refs`.
#[test]
fn gc_drops_stale_ids() {
    let mut state = DashboardState::new();
    state.pinned.insert(DashboardRowId::TopLevel(AgentId(7)));
    state.reorder.push(DashboardRowId::TopLevel(AgentId(7)));
    // Alive predicate says agent 7 no longer exists.
    state.gc_stale_refs(&|_| false);
    assert!(state.pinned.is_empty());
    assert!(state.reorder.is_empty());
}

/// Lenient parsing: malformed `pinned` (string instead of array)
/// is silently dropped. Edge case 12.
#[test]
fn parse_persist_keys_rejects_non_array() {
    let s = r#"pinned = "not-an-array""#;
    let doc: toml_edit::DocumentMut = s.parse().unwrap();
    let item = doc.get("pinned").unwrap();
    let out = parse_persist_keys(item);
    assert!(out.is_empty());
}

/// Lenient parsing: malformed `reorder` (table instead of array)
/// is silently dropped.
#[test]
fn parse_persist_key_list_rejects_non_array() {
    let s = "[reorder]\nx = 1";
    let doc: toml_edit::DocumentMut = s.parse().unwrap();
    let item = doc.get("reorder").unwrap();
    let out = parse_persist_key_list(item);
    assert!(out.is_empty());
}

/// Lenient parsing: array entries that aren't strings get dropped.
#[test]
fn parse_persist_keys_skips_non_string_entries() {
    let s = "pinned = [1, 2, \"top:sess-7\", false]";
    let doc: toml_edit::DocumentMut = s.parse().unwrap();
    let item = doc.get("pinned").unwrap();
    let out = parse_persist_keys(item);
    assert_eq!(out.len(), 1);
    assert!(out.contains(&PersistedRowId::TopLevel {
        session_id: "sess-7".into(),
    }));
}

/// Array entries past `MAX_PERSISTED_ENTRIES` are dropped.
#[test]
fn parse_persist_keys_caps_entry_count() {
    let many: Vec<String> = (0..(MAX_PERSISTED_ENTRIES * 2))
        .map(|i| format!("\"top:s-{i}\""))
        .collect();
    let s = format!("pinned = [{}]", many.join(","));
    let doc: toml_edit::DocumentMut = s.parse().unwrap();
    let item = doc.get("pinned").unwrap();
    let out = parse_persist_keys(item);
    assert!(out.len() <= MAX_PERSISTED_ENTRIES);
}

/// Each persisted key value is capped in length.
#[test]
fn parse_persist_keys_rejects_overlong_strings() {
    let huge = format!("\"top:{}\"", "a".repeat(MAX_PERSIST_KEY_LEN + 10));
    let s = format!("pinned = [{huge}]");
    let doc: toml_edit::DocumentMut = s.parse().unwrap();
    let item = doc.get("pinned").unwrap();
    let out = parse_persist_keys(item);
    assert!(out.is_empty());
}

/// Pin/unpin toggling works against the in-memory set.
#[test]
fn pin_unpin_toggle() {
    let mut state = DashboardState::new();
    let id = DashboardRowId::TopLevel(AgentId(1));
    state.selected = Some(id.clone());
    assert!(state.pinned.is_empty());
    state.toggle_pin_selected();
    assert!(state.pinned.contains(&id));
    state.toggle_pin_selected();
    assert!(state.pinned.is_empty());
}

/// Grouping toggle round-trips State → Directory → State.
#[test]
fn grouping_toggles() {
    let mut state = DashboardState::new();
    assert_eq!(state.grouping, Grouping::State);
    state.toggle_grouping();
    assert_eq!(state.grouping, Grouping::Directory);
    state.toggle_grouping();
    assert_eq!(state.grouping, Grouping::State);
}

/// `Ctrl+G` (the rebound grouping chord) emits `DashboardToggleGrouping`,
/// and `Ctrl+S` no longer toggles grouping — it's now "send + open".
#[test]
fn ctrl_g_toggles_grouping_ctrl_s_does_not() {
    use crate::app::actions::Action;
    let reg = crate::actions::ActionRegistry::defaults();
    let mut state = DashboardState::new();
    let ctrl_g = KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL);
    assert!(
        matches!(
            state.handle_key(&ctrl_g, &reg),
            InputOutcome::Action(Action::DashboardToggleGrouping)
        ),
        "Ctrl+G must emit DashboardToggleGrouping",
    );

    // Ctrl+S on the empty `[+ New Agent]` button is "send + open"
    // (create + detail), NOT a grouping toggle.
    let ctrl_s = KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL);
    assert!(
        matches!(
            state.handle_key(&ctrl_s, &reg),
            InputOutcome::Action(Action::DashboardCreateNewAgentWithDetail)
        ),
        "Ctrl+S must be send+open, not grouping",
    );
}

/// Edge case 4: selection survives a row refresh as long as the
/// underlying `DashboardRowId` is still present.
#[test]
fn reanchor_selection_keeps_existing_id() {
    let mut state = DashboardState::new();
    let id1 = DashboardRowId::TopLevel(AgentId(1));
    state.selected = Some(id1.clone());
    let rows = vec![super::super::row::DashboardRow {
        id: id1.clone(),
        label: "r1".to_string(),
        subtitle: None,
        state: RowState::Idle,
        activity: None,
        secondary_line: None,
        cwd_display: String::new(),
        cwd: std::path::PathBuf::from("/"),
        last_change_at: std::time::SystemTime::now(),
        pinned: false,
        is_active: false,
        badges: Vec::new(),
        context_pct: None,
        indent: 0,
        parent_label: None,
        is_more_placeholder: false,
        more_count: 0,
    }];
    state.reanchor_selection(&rows);
    assert_eq!(state.selected, Some(id1));
}

/// When the previous selection has disappeared,
/// `reanchor_selection` now drops the cursor to `None`
/// instead of auto-promoting to the first row. The "no row
/// selected → dispatch creates a new session" contract
/// depends on `None` being a stable steady state — a stale
/// agent vanishing must not silently re-arm the reply path
/// against whatever happens to be at the top.
#[test]
fn reanchor_selection_drops_to_none_when_previous_disappeared() {
    let mut state = DashboardState::new();
    state.selected = Some(DashboardRowId::TopLevel(AgentId(99)));
    let id1 = DashboardRowId::TopLevel(AgentId(1));
    let rows = vec![super::super::row::DashboardRow {
        id: id1.clone(),
        label: "r1".to_string(),
        subtitle: None,
        state: RowState::Idle,
        activity: None,
        secondary_line: None,
        cwd_display: String::new(),
        cwd: std::path::PathBuf::from("/"),
        last_change_at: std::time::SystemTime::now(),
        pinned: false,
        is_active: false,
        badges: Vec::new(),
        context_pct: None,
        indent: 0,
        parent_label: None,
        is_more_placeholder: false,
        more_count: 0,
    }];
    state.reanchor_selection(&rows);
    assert_eq!(
        state.selected, None,
        "stale selection must drop to None so the new-session path stays reachable",
    );
}

/// Real on-disk round-trip via the new
/// `write_persisted_to_path` / `load_persisted_from_path` helpers
/// plus `tempfile::TempDir`.
#[test]
fn persisted_on_disk_round_trip() {
    use std::collections::BTreeSet;
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("config.toml");
    let mut pinned: BTreeSet<PersistedRowId> = BTreeSet::new();
    pinned.insert(PersistedRowId::TopLevel {
        session_id: "sess-3".into(),
    });
    let reorder = vec![PersistedRowId::Subagent {
        parent_session_id: "sess-3".into(),
        child_session_id: "child-1".into(),
    }];
    let p = PersistedDashboard {
        enabled: false,
        grouping: Grouping::Directory,
        pinned: pinned.clone(),
        reorder: reorder.clone(),
    };
    write_persisted_to_path(&path, &p).unwrap();
    let loaded = load_persisted_from_path(&path).expect("must load back");
    assert!(!loaded.enabled);
    assert_eq!(loaded.grouping, Grouping::Directory);
    assert_eq!(loaded.pinned, pinned);
    assert_eq!(loaded.reorder, reorder);
}

/// The onboarding hint was removed — a stale `[dashboard.onboarding]`
/// table left by an older version is dropped on the next write.
#[test]
fn persisted_write_drops_stale_onboarding_table() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("config.toml");
    std::fs::write(
        &path,
        "[dashboard]\nenabled = true\n\n[dashboard.onboarding]\ndismissed = true\n",
    )
    .unwrap();
    write_persisted_to_path(&path, &PersistedDashboard::defaults()).unwrap();
    let after = std::fs::read_to_string(&path).unwrap();
    assert!(
        !after.contains("onboarding"),
        "stale onboarding table must be removed, got: {after:?}"
    );
}

/// Pre-populated `[hints]` table survives the dashboard write
/// (the guarantee — we never clobber unrelated tables).
#[test]
fn persisted_write_preserves_other_tables() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("config.toml");
    std::fs::write(
        &path,
        "[hints]\nmemory_modal_fullscreen = true\n\n[ui]\ncompact_mode = false\n",
    )
    .unwrap();
    let p = PersistedDashboard {
        enabled: true,
        grouping: Grouping::State,
        pinned: BTreeSet::new(),
        reorder: Vec::new(),
    };
    write_persisted_to_path(&path, &p).unwrap();
    let after = std::fs::read_to_string(&path).unwrap();
    assert!(after.contains("[hints]"));
    assert!(after.contains("memory_modal_fullscreen = true"));
    assert!(after.contains("[ui]"));
    assert!(after.contains("compact_mode = false"));
    assert!(after.contains("[dashboard]"));
}

/// Garbage `enabled` value falls back to defaults at load.
#[test]
fn persisted_load_garbage_enabled_falls_back() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("config.toml");
    std::fs::write(&path, "[dashboard]\nenabled = \"garbage\"\n").unwrap();
    let loaded = load_persisted_from_path(&path).expect("section present");
    // Garbage → default `true`.
    assert!(loaded.enabled);
    assert_eq!(loaded.grouping, Grouping::State);
    assert!(loaded.pinned.is_empty());
}

/// Write refuses to clobber a non-empty
/// unparseable file (round-trip with file containing garbage).
#[test]
fn persisted_write_refuses_unparseable_file() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("config.toml");
    std::fs::write(&path, "this is :: not :: valid :: toml :: at all").unwrap();
    let p = PersistedDashboard {
        enabled: true,
        grouping: Grouping::Directory,
        pinned: BTreeSet::new(),
        reorder: Vec::new(),
    };
    // write_persisted_to_path returns Ok(()) but does NOT overwrite.
    write_persisted_to_path(&path, &p).unwrap();
    let after = std::fs::read_to_string(&path).unwrap();
    assert!(
        after.starts_with("this is"),
        "file must be preserved verbatim; got: {after:?}"
    );
}

/// gc_stale_refs preserves the order of remaining
/// reorder entries.
#[test]
fn gc_preserves_order_of_remaining_reorder_entries() {
    let mut state = DashboardState::new();
    let alive1 = DashboardRowId::TopLevel(AgentId(1));
    let alive2 = DashboardRowId::TopLevel(AgentId(2));
    let alive3 = DashboardRowId::TopLevel(AgentId(3));
    let stale99 = DashboardRowId::TopLevel(AgentId(99));
    let stale100 = DashboardRowId::TopLevel(AgentId(100));
    state.reorder = vec![
        alive1.clone(),
        stale99.clone(),
        alive2.clone(),
        stale100.clone(),
        alive3.clone(),
    ];
    let alive_set: std::collections::HashSet<_> = [alive1.clone(), alive2.clone(), alive3.clone()]
        .into_iter()
        .collect();
    state.gc_stale_refs(&|id| alive_set.contains(id));
    assert_eq!(state.reorder, vec![alive1, alive2, alive3]);
}

/// Two different pinned rows coexist.
#[test]
fn pin_two_different_rows_coexist() {
    let mut state = DashboardState::new();
    let id_a = DashboardRowId::TopLevel(AgentId(1));
    let id_b = DashboardRowId::TopLevel(AgentId(2));
    state.selected = Some(id_a.clone());
    state.toggle_pin_selected();
    state.selected = Some(id_b.clone());
    state.toggle_pin_selected();
    assert!(state.pinned.contains(&id_a));
    assert!(state.pinned.contains(&id_b));
    assert_eq!(state.pinned.len(), 2);
}

/// Grouping toggle idempotency over multiple cycles.
#[test]
fn grouping_toggles_three_times() {
    let mut state = DashboardState::new();
    let start = state.grouping;
    state.toggle_grouping();
    state.toggle_grouping();
    state.toggle_grouping();
    assert_ne!(state.grouping, start);
    state.toggle_grouping();
    assert_eq!(state.grouping, start);
}

/// parse_row_state_token covers all documented synonyms.
#[test]
fn parse_row_state_token_all_synonyms() {
    for (s, expected) in [
        ("needs-input", RowState::NeedsInput),
        ("needs_input", RowState::NeedsInput),
        ("needsinput", RowState::NeedsInput),
        ("needs", RowState::NeedsInput),
        ("input", RowState::NeedsInput),
        ("NEEDS-INPUT", RowState::NeedsInput),
        ("working", RowState::Working),
        ("busy", RowState::Working),
        ("running", RowState::Working),
        ("idle", RowState::Idle),
        ("IDLE", RowState::Idle),
        ("inactive", RowState::Inactive),
        ("dormant", RowState::Inactive),
        ("completed", RowState::Completed),
        ("done", RowState::Completed),
        ("failed", RowState::Failed),
        ("errored", RowState::Failed),
        ("cancelled", RowState::Failed),
        ("canceled", RowState::Failed),
        ("blocked", RowState::Blocked),
        ("paused", RowState::Blocked),
    ] {
        assert_eq!(parse_row_state_token(s), Some(expected), "input={s}");
    }
    // Empty and whitespace return None.
    assert_eq!(parse_row_state_token(""), None);
    assert_eq!(parse_row_state_token("   "), None);
    assert_eq!(parse_row_state_token("nonsense"), None);
}

/// Rename cap is honored exactly.
#[test]
fn rename_at_cap_drops_extra_char() {
    assert_eq!(
        MAX_RENAME_SCALARS,
        echo_agent_runtime::session::persistence::MAX_TITLE_SCALARS
    );
    assert_eq!(
        echo_agent_runtime::session::persistence::MAX_TITLE_SCALARS,
        100
    );
    let mut draft = RenameDraft::new(
        DashboardRowId::TopLevel(AgentId(0)),
        "a".repeat(MAX_RENAME_SCALARS),
    );
    let key = KeyEvent::new(KeyCode::Char('b'), KeyModifiers::NONE);
    let outcome = handle_rename_key(&mut draft, &key);
    assert!(matches!(outcome, InputOutcome::Changed));
    assert_eq!(draft.text().chars().count(), MAX_RENAME_SCALARS);
    assert!(
        draft.text().ends_with('a'),
        "char at cap should NOT be replaced: got {:?}",
        draft.text()
    );
}

/// under-cap appends correctly.
#[test]
fn rename_under_cap_appends() {
    let mut draft = RenameDraft::new(
        DashboardRowId::TopLevel(AgentId(0)),
        "a".repeat(MAX_RENAME_SCALARS - 1),
    );
    let key = KeyEvent::new(KeyCode::Char('b'), KeyModifiers::NONE);
    let outcome = handle_rename_key(&mut draft, &key);
    assert!(matches!(outcome, InputOutcome::Changed));
    assert_eq!(draft.text().chars().count(), MAX_RENAME_SCALARS);
    assert!(draft.text().ends_with('b'));
}

/// Ctrl+letter in rename mode rejected (does not type
/// the bare letter into the draft); Ctrl+C cancels.
#[test]
fn rename_rejects_ctrl_chars() {
    let mut draft = RenameDraft::new(DashboardRowId::TopLevel(AgentId(0)), "hello");
    let ctrl_r = KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL);
    let outcome = handle_rename_key(&mut draft, &ctrl_r);
    assert!(matches!(outcome, InputOutcome::Unchanged));
    assert_eq!(draft.text(), "hello", "draft must not gain 'r'");
    // Ctrl+C → cancel.
    let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
    let outcome = handle_rename_key(&mut draft, &ctrl_c);
    assert!(matches!(
        outcome,
        InputOutcome::Action(crate::app::actions::Action::DashboardCancelRename)
    ));
}

#[test]
fn rename_word_motion_is_canonical_and_cursor_only() {
    for key in [
        KeyEvent::new(KeyCode::Left, KeyModifiers::ALT),
        KeyEvent::new(KeyCode::Char('b'), KeyModifiers::ALT),
        KeyEvent::new(KeyCode::Left, KeyModifiers::CONTROL),
    ] {
        let mut draft = RenameDraft::new(DashboardRowId::TopLevel(AgentId(0)), "hello-world");
        let outcome = handle_rename_key(&mut draft, &key);
        assert!(matches!(outcome, InputOutcome::Changed));
        assert_eq!(draft.text(), "hello-world");
        assert_eq!(draft.cursor_byte(), "hello-".len());
    }

    for key in [
        KeyEvent::new(KeyCode::Right, KeyModifiers::ALT),
        KeyEvent::new(KeyCode::Char('f'), KeyModifiers::ALT),
    ] {
        let mut draft = RenameDraft::new(DashboardRowId::TopLevel(AgentId(0)), "hello-world");
        let _ = handle_rename_key(
            &mut draft,
            &KeyEvent::new(KeyCode::Home, KeyModifiers::NONE),
        );
        let outcome = handle_rename_key(&mut draft, &key);
        assert!(matches!(outcome, InputOutcome::Changed));
        assert_eq!(draft.cursor_byte(), "hello".len());
    }

    let mut draft = RenameDraft::new(DashboardRowId::TopLevel(AgentId(0)), "hello-world");
    let outcome = handle_rename_key(
        &mut draft,
        &KeyEvent::new(KeyCode::Backspace, KeyModifiers::ALT),
    );
    assert!(matches!(outcome, InputOutcome::Changed));
    assert_eq!(draft.text(), "hello-");
}

#[test]
fn rename_grapheme_delete_and_middle_insert() {
    let grapheme = "👩🏽\u{200d}💻";
    let mut draft = RenameDraft::new(
        DashboardRowId::TopLevel(AgentId(0)),
        format!("a{grapheme}b"),
    );
    let _ = handle_rename_key(
        &mut draft,
        &KeyEvent::new(KeyCode::Home, KeyModifiers::NONE),
    );
    let _ = handle_rename_key(
        &mut draft,
        &KeyEvent::new(KeyCode::Right, KeyModifiers::NONE),
    );
    let outcome = handle_rename_key(
        &mut draft,
        &KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE),
    );
    assert!(matches!(outcome, InputOutcome::Changed));
    assert_eq!(draft.text(), "ab");

    let outcome = handle_rename_key(
        &mut draft,
        &KeyEvent::new(KeyCode::Char('X'), KeyModifiers::NONE),
    );
    assert!(matches!(outcome, InputOutcome::Changed));
    assert_eq!(draft.text(), "aXb");
}

#[test]
fn rename_policy_and_paste_preserve_scalar_cap() {
    let mut draft = RenameDraft::new(
        DashboardRowId::TopLevel(AgentId(0)),
        "a".repeat(MAX_RENAME_SCALARS - 1),
    );
    let outcome = handle_rename_key(
        &mut draft,
        &KeyEvent::new(KeyCode::Char('\u{202e}'), KeyModifiers::NONE),
    );
    assert!(matches!(outcome, InputOutcome::Changed));
    assert_eq!(draft.text().chars().count(), MAX_RENAME_SCALARS - 1);

    let outcome = handle_rename_paste(&mut draft, "中\r\n文");
    assert!(matches!(outcome, InputOutcome::Changed));
    assert_eq!(draft.text().chars().count(), MAX_RENAME_SCALARS);
    assert!(draft.text().ends_with('中'));
}

#[test]
fn modified_enter_does_not_commit_rename() {
    let mut draft = RenameDraft::new(DashboardRowId::TopLevel(AgentId(0)), "name");
    for modifiers in [KeyModifiers::ALT, KeyModifiers::SHIFT] {
        let outcome = handle_rename_key(&mut draft, &KeyEvent::new(KeyCode::Enter, modifiers));
        assert!(!matches!(
            outcome,
            InputOutcome::Action(Action::DashboardCommitRename)
        ));
    }
}

#[test]
fn rename_paste_preserves_emoji_zwj_sequences() {
    let mut draft = RenameDraft::new(DashboardRowId::TopLevel(AgentId(0)), "");
    let outcome = handle_rename_paste(&mut draft, "👩‍💻");
    assert!(matches!(outcome, InputOutcome::Changed));
    assert_eq!(draft.text(), "👩‍💻");
}

#[test]
fn rename_mode_routes_bracketed_paste_only_to_rename_editor() {
    let mut state = DashboardState::new();
    state.dispatch.set_text("hidden dispatch");
    state.rename = Some(RenameDraft::new(DashboardRowId::TopLevel(AgentId(0)), "ab"));
    let registry = crate::actions::ActionRegistry::defaults();
    let _ = state.handle_input(
        &Event::Key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE)),
        &registry,
    );
    let outcome = state.handle_input(&Event::Paste("中\r\n".to_owned()), &registry);
    assert!(matches!(outcome, InputOutcome::Changed));
    assert_eq!(state.rename.as_ref().map(RenameDraft::text), Some("a中b"));
    assert_eq!(state.dispatch.text(), "hidden dispatch");
}

/// Esc-cancelling the worktree-label dialog must restore the stashed
/// prompt (from the prompt-send path) to the dispatch input instead of
/// silently discarding the user's typed text. Mirrors the restore in
/// `dispatch_dashboard_confirm_worktree`'s not-a-repo error path.
#[test]
fn worktree_dialog_cancel_restores_stashed_prompt_state() {
    let mut state = DashboardState::new();
    let reg = crate::actions::ActionRegistry::defaults();
    // Simulate the prompt-send path: the prompt is stashed, the dialog
    // is opened, and the dispatch input is cleared.
    state.dispatch.set_text("fix the bug ");
    let draft_end = state.dispatch.text().len();
    state.dispatch.set_cursor(draft_end);
    state.dispatch.insert_image(peek_test_image()).unwrap();
    state.pending_worktree_prompt = Some(state.dispatch.stash());
    state.worktree_dialog = Some(crate::app::app_view::NewWorktreeDialogState::new());
    state.dispatch.set_text("");

    let outcome = state.handle_input(
        &Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
        &reg,
    );

    assert!(matches!(outcome, InputOutcome::Changed));
    assert!(state.worktree_dialog.is_none(), "Esc closes the dialog");
    assert!(
        state.pending_worktree_prompt.is_none(),
        "the stash must be consumed",
    );
    assert_eq!(
        state.dispatch.text(),
        "fix the bug [Image #1] ",
        "the stashed prompt must be restored to the dispatch input",
    );
    assert_eq!(state.dispatch.drain_images().len(), 1);
}

/// Cancelling the dialog when it was opened from the `[+ New Agent]`
/// button (no stashed prompt) leaves the dispatch input untouched.
#[test]
fn worktree_dialog_cancel_without_stash_leaves_input_empty() {
    let mut state = DashboardState::new();
    let reg = crate::actions::ActionRegistry::defaults();
    state.worktree_dialog = Some(crate::app::app_view::NewWorktreeDialogState::new());
    state.dispatch.set_text("");

    let _ = state.handle_input(
        &Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
        &reg,
    );

    assert!(state.worktree_dialog.is_none());
    assert!(state.pending_worktree_prompt.is_none());
    assert_eq!(
        state.dispatch.text(),
        "",
        "no stash → dispatch input stays empty",
    );
}

/// `Ctrl+W` resolves to the worktree-toggle action (which is what puts it
/// in the dashboard cheatsheet and lets the dispatcher git-gate it). The
/// actual flag flip + non-git guard live in
/// `dispatch_dashboard_toggle_worktree` and are covered there.
#[test]
fn ctrl_w_emits_toggle_worktree_action() {
    let mut state = DashboardState::new();
    let reg = crate::actions::ActionRegistry::defaults();

    let outcome = state.handle_input(
        &Event::Key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL)),
        &reg,
    );
    assert!(
        matches!(
            outcome,
            InputOutcome::Action(Action::DashboardToggleWorktree)
        ),
        "Ctrl+W must resolve to the DashboardToggleWorktree action",
    );
}

// ---------------------------------------------------------------
// handle_key tests (Esc cascade, Enter routing).
// ---------------------------------------------------------------

fn make_state_with_selection() -> DashboardState {
    let mut s = DashboardState::new();
    s.selected = Some(DashboardRowId::TopLevel(AgentId(0)));
    s
}

fn peek_fields_for_test(response_type: &str) -> super::super::peek::PeekFields {
    super::super::peek::PeekFields {
        label: "x".to_string(),
        time_ago: String::new(),
        response_type: response_type.to_string(),
        last_user_message: None,
        question: None,
        options: Vec::new(),
        request_id: None,
        reject_option: None,
    }
}

/// edge case 13: Esc closes peek first.
#[test]
fn esc_closes_peek_first() {
    let mut state = make_state_with_selection();
    state.peek = Some(super::super::peek::PeekPanelState::new(
        DashboardRowId::TopLevel(AgentId(0)),
        peek_fields_for_test("Idle"),
    ));
    let reg = crate::actions::ActionRegistry::defaults();
    let key = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
    let outcome = state.handle_key(&key, &reg);
    assert!(matches!(outcome, InputOutcome::Changed));
    assert!(state.peek.is_none());
}

fn state_with_open_peek() -> DashboardState {
    // PeekPanelState::new seeds focus from load_vim_mode(); pin off so
    // older tests that assume a focused reply don't depend on config /
    // process cache. Vim tests set true and restore themselves.
    crate::appearance::cache::set_vim_mode(false);
    let mut s = make_state_with_selection();
    s.peek = Some(super::super::peek::PeekPanelState::new(
        DashboardRowId::TopLevel(AgentId(0)),
        peek_fields_for_test("Idle"),
    ));
    s
}

/// Regression: with the peek open but the reply UNFOCUSED (Tab → row
/// nav), generic editing chords must NOT leak into the hidden new-session
/// dispatch draft behind the panel. Backspace / Delete are consumed
/// (`Unchanged`) instead of falling through to the hidden dispatch widget.
/// (Ctrl+W is NOT tested here — it's a registry-bound dashboard chord, the
/// worktree toggle, so like Ctrl+X it intentionally falls through to fire
/// its action with the peek open.)
#[test]
fn peek_unfocused_editing_chords_do_not_leak_to_dispatch() {
    let mut state = state_with_open_peek();
    let reg = crate::actions::ActionRegistry::defaults();
    // Hidden new-session draft, caret at END (where Backspace bites;
    // set_text alone parks it at 0).
    state.dispatch.set_text("hidden draft");
    state.dispatch.set_cursor(state.dispatch.text().len());
    // Tab → unfocus the reply (it becomes a row-nav surface).
    let _ = state.handle_key(&KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE), &reg);
    assert!(!state.peek.as_ref().unwrap().focused, "Tab must unfocus");

    for key in [
        KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE),
        KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE),
    ] {
        let outcome = state.handle_key(&key, &reg);
        assert!(
            matches!(outcome, InputOutcome::Unchanged),
            "{key:?} must be consumed (Unchanged) with the peek open, got {outcome:?}",
        );
    }
    assert_eq!(
        state.dispatch.text(),
        "hidden draft",
        "editing chords must NOT leak into the hidden dispatch draft",
    );
    assert!(
        !state.peek.as_ref().unwrap().focused,
        "consumed editing chords must not grab focus",
    );
}

/// While the peek panel is open, bare printable keys type into the
/// `❯ reply` input (not the hidden dispatch box).
#[test]
fn peek_typing_edits_reply_buffer() {
    let mut state = state_with_open_peek();
    let reg = crate::actions::ActionRegistry::defaults();
    for c in ['h', 'i'] {
        let key = KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);
        let _ = state.handle_key(&key, &reg);
    }
    assert_eq!(state.peek_reply.text(), "hi");
    // The dispatch (new-session) buffer is untouched.
    assert!(state.dispatch.text().is_empty());
}

/// Enter with a typed reply emits `DashboardPeekReply` (no attach);
/// Ctrl+S ("send + open") sets `attach=true`.
#[test]
fn peek_enter_with_text_emits_reply() {
    use crate::app::actions::Action;
    let mut state = state_with_open_peek();
    state.peek_reply.set_text("ship it");
    let reg = crate::actions::ActionRegistry::defaults();
    let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
    match state.handle_key(&enter, &reg) {
        InputOutcome::Action(Action::DashboardPeekReply { text, attach, row }) => {
            assert_eq!(text, "ship it");
            assert!(!attach);
            assert_eq!(row, DashboardRowId::TopLevel(AgentId(0)));
        }
        other => panic!("expected DashboardPeekReply, got {other:?}"),
    }

    let ctrl_s = KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL);
    match state.handle_key(&ctrl_s, &reg) {
        InputOutcome::Action(Action::DashboardPeekReply { attach, text, .. }) => {
            assert!(attach, "Ctrl+S must set attach=true (send + open)");
            assert_eq!(text, "ship it");
        }
        other => panic!("expected DashboardPeekReply, got {other:?}"),
    }
}

fn peek_test_image() -> crate::prompt_images::PastedImage {
    crate::prompt_images::PastedImage {
        element_id: echo_agent_ratatui_textarea::ElementId::from_raw(0),
        display_number: 0,
        mime_type: "image/png".into(),
        dimensions: Some((10, 10)),
        byte_len: 16,
        encoded_bytes: Some(vec![0u8; 16].into()),
        source_path: Some(std::path::PathBuf::from(
            "/Users/somebody/very/long/path/screenshot.png",
        )),
        staged_temp_path: None,
        session_image_path: None,
        preview: crate::prompt_images::PromptImagePreview::default(),
    }
}

fn write_test_png(dir: &std::path::Path) -> std::path::PathBuf {
    let img: image::ImageBuffer<image::Rgba<u8>, Vec<u8>> =
        image::ImageBuffer::from_pixel(16, 16, image::Rgba([255, 0, 0, 255]));
    let path = dir.join("shot.png");
    img.save_with_format(&path, image::ImageFormat::Png)
        .unwrap();
    path
}

fn test_png_bytes() -> Vec<u8> {
    let img: image::ImageBuffer<image::Rgba<u8>, Vec<u8>> =
        image::ImageBuffer::from_pixel(16, 16, image::Rgba([0, 128, 255, 255]));
    let mut buf = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png)
        .unwrap();
    buf
}

/// The Ctrl/Cmd+V chord path attaches a clipboard image to the peek
/// reply (image wins over the caption) without leaking into dispatch. Drives
/// the real deferred entry point + completion.
#[test]
fn peek_paste_key_clipboard_image_wins_over_text() {
    let mut state = state_with_open_peek();
    cmd_v_image(&mut state, Some("ignored text"));
    assert_eq!(state.peek_reply.images.len(), 1);
    let text = state.peek_reply.text();
    assert!(text.contains("[Image #1]"), "got {text:?}");
    assert!(
        !text.contains("ignored text"),
        "text won over image: {text:?}"
    );
    assert!(state.dispatch.images.is_empty() && state.dispatch.text().is_empty());
}

/// In question mode the chord path must not defer/attach a clipboard
/// image — the reply is text-only on the wire.
#[test]
fn peek_paste_key_question_mode_blocks_clipboard_image() {
    let mut state = state_with_open_peek();
    if let Some(p) = state.peek.as_mut() {
        p.focused = true;
        p.question = Some("Allow?".into());
        p.options = vec![("yes".into(), "Yes".into()), ("no".into(), "No".into())];
        p.reject_option = Some(1);
        p.selected_option = Some(1);
    }
    let reg = crate::actions::ActionRegistry::defaults();
    // Even with a raster on the pasteboard, question mode must not probe.
    crate::clipboard::set_clipboard_probe_hook(crate::clipboard::ClipboardProbeHook::with_raster(
        None,
    ));
    let _ = state.handle_input(&ctrl_v_event(), &reg);
    let deferred = deferred_probe_target(&state).is_some();
    crate::clipboard::clear_clipboard_probe_hook();
    assert!(!deferred, "question mode must not defer an image probe");
    assert!(state.peek_reply.images.is_empty());
    assert!(!state.peek_reply.text().contains("[Image #"));
}

/// A whitespace-only chord paste inserts no text into the reply. Trimmed-empty
/// routes to the FileUrlsThenImage probe (to catch an image-only pasteboard),
/// so it defers rather than inserting spaces.
#[test]
fn peek_paste_key_whitespace_only_is_unchanged() {
    let mut state = state_with_open_peek();
    let reg = crate::actions::ActionRegistry::defaults();
    crate::clipboard::set_clipboard_probe_hook(crate::clipboard::ClipboardProbeHook::no_raster(
        Some("   "),
    ));
    let _ = state.handle_input(&ctrl_v_event(), &reg);
    crate::clipboard::clear_clipboard_probe_hook();
    assert!(state.peek_reply.text().is_empty());
}

/// An empty-clipboard chord defers a probe (to catch a Finder file-url /
/// image-only pasteboard) without inserting text.
#[test]
fn peek_paste_key_empty_clipboard_defers_probe() {
    let mut state = state_with_open_peek();
    let reg = crate::actions::ActionRegistry::defaults();
    crate::clipboard::set_clipboard_probe_hook(crate::clipboard::ClipboardProbeHook::no_raster(
        None,
    ));
    let _ = state.handle_input(&ctrl_v_event(), &reg);
    let target = deferred_probe_target(&state);
    crate::clipboard::clear_clipboard_probe_hook();
    assert!(
        matches!(
            target,
            Some(crate::app::actions::ClipboardPasteTarget::DashboardPeek { .. })
        ),
        "empty pbpaste still probes for a file-url / image via the peek target"
    );
    assert!(state.peek_reply.text().is_empty());
}

#[test]
fn dashboard_failed_text_read_is_carried_into_deferred_context() {
    let mut state = DashboardState::new();
    let reg = crate::actions::ActionRegistry::defaults();
    crate::clipboard::set_clipboard_probe_hook(crate::clipboard::ClipboardProbeHook {
        text_read_failed: true,
        ..crate::clipboard::ClipboardProbeHook::snapshot_unavailable()
    });

    let _ = state.handle_input(&ctrl_v_event(), &reg);
    let ctx = deferred_probe_ctx(&state).expect("failed text read must probe attachments");
    crate::clipboard::clear_clipboard_probe_hook();

    assert!(ctx.source.text_read_failed());
}

/// A real path-image paste routed through `handle_input`
/// attaches an `[Image #N]` chip on the peek reply (not the hidden
/// dispatch input), with the source path stripped for a clean chip.
#[test]
fn peek_path_paste_routes_image_to_reply_not_dispatch() {
    let dir = tempfile::tempdir().unwrap();
    let png = write_test_png(dir.path());
    let mut state = state_with_open_peek();
    let reg = crate::actions::ActionRegistry::defaults();
    // Trailing newline skips the clipboard probe so the test is hermetic.
    let paste = format!("{}\n", png.display());
    let outcome = state.handle_input(&Event::Paste(paste), &reg);
    assert!(matches!(outcome, InputOutcome::Changed));
    let text = state.peek_reply.text();
    assert!(text.contains("[Image #1]"), "expected chip, got {text:?}");
    assert!(
        !text.contains("shot.png"),
        "chip must not embed the source path, got {text:?}"
    );
    assert_eq!(state.peek_reply.images.len(), 1);
    assert!(
        state.dispatch.text().is_empty() && state.dispatch.images.is_empty(),
        "image must not leak into the hidden dispatch input"
    );
}

/// In question mode the reply is text-only on the wire — a
/// path-image paste must NOT become an image chip.
#[test]
fn peek_question_mode_path_paste_stays_text_only() {
    let dir = tempfile::tempdir().unwrap();
    let png = write_test_png(dir.path());
    let mut state = state_with_open_peek();
    let reg = crate::actions::ActionRegistry::defaults();
    if let Some(p) = state.peek.as_mut() {
        p.focused = true;
        p.question = Some("Allow?".into());
        p.options = vec![("yes".into(), "Yes".into()), ("no".into(), "No".into())];
        p.reject_option = Some(1);
        p.selected_option = Some(1);
    }
    let paste = format!("{}\n", png.display());
    let _ = state.handle_input(&Event::Paste(paste), &reg);
    assert!(state.peek_reply.images.is_empty());
    assert!(!state.peek_reply.text().contains("[Image #"));
}

/// `clear_peek_reply` drops image state with the draft text.
#[test]
fn clear_peek_reply_clears_images() {
    let mut state = state_with_open_peek();
    let _ = state.attach_peek_pasted_image(peek_test_image());
    assert!(!state.peek_reply.images.is_empty());
    state.clear_peek_reply();
    assert!(state.peek_reply.text().is_empty());
    assert!(state.peek_reply.images.is_empty());
}

/// Enter with an image chip on the reply emits `DashboardPeekReply`
/// carrying the chip placeholder text (images drain at dispatch time).
#[test]
fn peek_enter_with_image_emits_reply() {
    use crate::app::actions::Action;
    let mut state = state_with_open_peek();
    let _ = state.attach_peek_pasted_image(peek_test_image());
    let reg = crate::actions::ActionRegistry::defaults();
    let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
    match state.handle_key(&enter, &reg) {
        InputOutcome::Action(Action::DashboardPeekReply { text, attach, .. }) => {
            assert!(text.contains("[Image #1]"), "got {text:?}");
            assert!(!attach);
        }
        other => panic!("expected DashboardPeekReply, got {other:?}"),
    }
    // Action does not drain images — still on the widget until dispatch.
    assert_eq!(state.peek_reply.images.len(), 1);
}

/// Question/permission feedback is text-only — an image chip
/// left on the reply must not leak its `[Image #N]` token into the
/// submitted feedback text.
#[test]
fn peek_question_feedback_strips_image_placeholder() {
    use crate::app::actions::Action;
    let mut state = state_with_open_peek();
    state.peek_reply.set_text("no thanks");
    state.peek_reply.set_cursor(state.peek_reply.text().len());
    let _ = state.attach_peek_pasted_image(peek_test_image());
    assert!(state.peek_reply.text().contains("[Image #1]"));
    if let Some(p) = state.peek.as_mut() {
        p.focused = true;
        p.question = Some("Allow?".into());
        p.options = vec![("yes".into(), "Yes".into()), ("no".into(), "No".into())];
        p.reject_option = Some(1);
        p.selected_option = Some(1);
        p.request_id = Some(7);
    }
    let reg = crate::actions::ActionRegistry::defaults();
    let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
    match state.handle_key(&enter, &reg) {
        InputOutcome::Action(Action::DashboardPermissionFollowup { text, .. }) => {
            assert!(!text.contains("[Image #"), "image token leaked: {text:?}");
            assert!(
                text.contains("no thanks"),
                "feedback text dropped: {text:?}"
            );
        }
        other => panic!("expected DashboardPermissionFollowup, got {other:?}"),
    }
}

/// The Ask "Other" fallthrough (image-only draft, no typed text)
/// must also strip the `[Image #N]` token from the freeform answer.
#[test]
fn peek_ask_other_image_only_strips_placeholder() {
    use crate::app::actions::Action;
    let mut state = state_with_open_peek();
    let _ = state.attach_peek_pasted_image(peek_test_image());
    assert!(state.peek_reply.text().contains("[Image #1]"));
    if let Some(p) = state.peek.as_mut() {
        p.focused = true;
        p.question = Some("Which?".into());
        p.options = vec![("a".into(), "A".into()), ("other".into(), "Other".into())];
        p.reject_option = Some(1);
        p.selected_option = Some(1);
        p.request_id = None; // Ask tool (not a permission)
    }
    let reg = crate::actions::ActionRegistry::defaults();
    let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
    match state.handle_key(&enter, &reg) {
        InputOutcome::Action(Action::DashboardQuestionAnswer { freeform, .. }) => {
            assert!(
                !freeform.contains("[Image #"),
                "image token leaked: {freeform:?}"
            );
        }
        other => panic!("expected DashboardQuestionAnswer, got {other:?}"),
    }
}

/// Shift+Enter / Alt+Enter insert a newline into the focused peek
/// reply (multiline compose) instead of sending — the reply text
/// grows and no action is emitted.
#[test]
fn peek_shift_enter_inserts_newline() {
    let mut state = state_with_open_peek();
    let reg = crate::actions::ActionRegistry::defaults();
    state.peek_reply.set_text("line one");
    // Caret at end so the newline appends.
    let shift_enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT);
    let outcome = state.handle_key(&shift_enter, &reg);
    assert!(
        matches!(outcome, InputOutcome::Changed),
        "Shift+Enter must edit the reply, got {outcome:?}",
    );
    assert!(
        state.peek_reply.text().contains('\n'),
        "Shift+Enter must insert a newline, got {:?}",
        state.peek_reply.text(),
    );
    // Alt+Enter does the same.
    let alt_enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT);
    let _ = state.handle_key(&alt_enter, &reg);
    assert_eq!(state.peek_reply.text().matches('\n').count(), 2);
}

/// With multiline_mode on, peek bare Enter inserts a newline; Shift+Enter sends.
#[test]
fn peek_multiline_mode_swaps_enter() {
    use crate::app::actions::Action;
    let mut state = state_with_open_peek();
    state.multiline_mode = true;
    if let Some(p) = state.peek.as_mut() {
        p.focused = true;
    }
    state.peek_reply.set_text("line one");
    let reg = crate::actions::ActionRegistry::defaults();

    let bare = state.handle_key(&KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &reg);
    assert!(
        matches!(bare, InputOutcome::Changed),
        "bare Enter in multiline peek must insert newline, got {bare:?}"
    );
    assert!(
        state.peek_reply.text().contains('\n'),
        "got {:?}",
        state.peek_reply.text()
    );

    let shift = state.handle_key(&KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT), &reg);
    match shift {
        InputOutcome::Action(Action::DashboardPeekReply {
            text,
            attach: false,
            ..
        }) => {
            assert!(text.contains("line one"), "peek reply text: {text:?}");
        }
        other => panic!("Shift+Enter in multiline peek must send, got {other:?}"),
    }
}

/// Enter with an empty reply opens the peeked agent rather than
/// sending an empty prompt.
#[test]
fn peek_enter_empty_opens_agent() {
    use crate::app::actions::Action;
    let mut state = state_with_open_peek();
    let reg = crate::actions::ActionRegistry::defaults();
    let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
    match state.handle_key(&enter, &reg) {
        InputOutcome::Action(Action::DashboardAttach(row)) => {
            assert_eq!(row, DashboardRowId::TopLevel(AgentId(0)));
        }
        other => panic!("expected DashboardAttach, got {other:?}"),
    }
}

/// Right arrow on a selected agent (peek open) opens its detail view
/// — the mirror of the agent overlay's Left-arrow back-out.
#[test]
fn peek_right_arrow_opens_agent() {
    use crate::app::actions::Action;
    let mut state = state_with_open_peek();
    let reg = crate::actions::ActionRegistry::defaults();
    let right = KeyEvent::new(KeyCode::Right, KeyModifiers::NONE);
    match state.handle_key(&right, &reg) {
        InputOutcome::Action(Action::DashboardAttach(row)) => {
            assert_eq!(row, DashboardRowId::TopLevel(AgentId(0)));
        }
        other => panic!("expected DashboardAttach, got {other:?}"),
    }
}

/// Right arrow with a non-empty FOCUSED reply moves the caret within
/// the draft instead of opening the agent — the reply text is
/// preserved and no attach is emitted.
#[test]
fn peek_right_arrow_with_text_moves_caret_not_open() {
    use crate::app::actions::Action;
    let mut state = state_with_open_peek();
    let reg = crate::actions::ActionRegistry::defaults();
    state.peek_reply.set_text("draft");
    let right = KeyEvent::new(KeyCode::Right, KeyModifiers::NONE);
    let outcome = state.handle_key(&right, &reg);
    assert!(
        !matches!(outcome, InputOutcome::Action(Action::DashboardAttach(_))),
        "Right with a focused non-empty reply must NOT open the agent, got {outcome:?}",
    );
    assert_eq!(
        state.peek_reply.text(),
        "draft",
        "Right must leave the reply draft intact",
    );
}

/// Up/Down switch the peeked agent (the panel follows the selection
/// cursor).
#[test]
fn peek_arrows_switch_selected_agent() {
    use crate::app::actions::Action;
    let mut state = state_with_open_peek();
    let reg = crate::actions::ActionRegistry::defaults();
    // Empty reply → arrows are a navigation surface.
    let down = KeyEvent::new(KeyCode::Down, KeyModifiers::NONE);
    assert!(matches!(
        state.handle_key(&down, &reg),
        InputOutcome::Action(Action::DashboardSelectNext)
    ));
    let up = KeyEvent::new(KeyCode::Up, KeyModifiers::NONE);
    assert!(matches!(
        state.handle_key(&up, &reg),
        InputOutcome::Action(Action::DashboardSelectPrev)
    ));
}

/// With a non-empty FOCUSED reply, bare Up/Down move the caret
/// WITHIN the reply text (multi-line draft) instead of switching the
/// peeked agent — they edit, never emit a `DashboardSelect*` action,
/// and leave the draft text untouched.
#[test]
fn peek_arrows_move_caret_when_reply_has_content() {
    let mut state = state_with_open_peek();
    let reg = crate::actions::ActionRegistry::defaults();
    // Two-line draft (caret at the start after set_text).
    state.peek_reply.set_text("line one\nline two");

    // Down must NOT switch agents — it moves the caret down a line.
    let down = state.handle_key(&KeyEvent::new(KeyCode::Down, KeyModifiers::NONE), &reg);
    assert!(
        !matches!(down, InputOutcome::Action(_)),
        "Down with reply content must edit the caret, not switch agents, got {down:?}",
    );
    // Up likewise stays within the input.
    let up = state.handle_key(&KeyEvent::new(KeyCode::Up, KeyModifiers::NONE), &reg);
    assert!(
        !matches!(up, InputOutcome::Action(_)),
        "Up with reply content must edit the caret, not switch agents, got {up:?}",
    );
    // The draft is untouched (caret moves only).
    assert_eq!(state.peek_reply.text(), "line one\nline two");
}

/// An UNFOCUSED peek (Tab → row nav) keeps Up/Down as agent-switch
/// even with reply content — the reply isn't the active surface.
#[test]
fn peek_arrows_switch_agent_when_unfocused_despite_content() {
    use crate::app::actions::Action;
    let mut state = state_with_open_peek();
    let reg = crate::actions::ActionRegistry::defaults();
    state.peek_reply.set_text("a draft");
    state.peek.as_mut().unwrap().focused = false; // Tab → row nav
    assert!(matches!(
        state.handle_key(&KeyEvent::new(KeyCode::Down, KeyModifiers::NONE), &reg),
        InputOutcome::Action(Action::DashboardSelectNext)
    ));
}

/// The (peek-less) dispatch input mirrors the peek: with content,
/// bare Up/Down move the caret within the text (no `DashboardSelect*`
/// emitted); with an EMPTY prompt they navigate the row list (browse
/// convenience).
#[test]
fn dispatch_arrows_move_caret_with_content_navigate_when_empty() {
    use crate::app::actions::Action;
    let reg = crate::actions::ActionRegistry::defaults();

    // Empty prompt → Up/Down navigate the list.
    let mut empty = make_state_with_selection();
    assert!(empty.dispatch.text().is_empty());
    assert!(matches!(
        empty.handle_key(&KeyEvent::new(KeyCode::Down, KeyModifiers::NONE), &reg),
        InputOutcome::Action(Action::DashboardSelectNext)
    ));

    // Non-empty multi-line prompt → Up/Down edit the caret, never
    // switching the selected row.
    let mut typed = make_state_with_selection();
    typed.dispatch.set_text("line one\nline two");
    let down = typed.handle_key(&KeyEvent::new(KeyCode::Down, KeyModifiers::NONE), &reg);
    assert!(
        !matches!(
            down,
            InputOutcome::Action(Action::DashboardSelectNext | Action::DashboardSelectPrev)
        ),
        "Down with dispatch content must move the caret, not the list, got {down:?}",
    );
    let up = typed.handle_key(&KeyEvent::new(KeyCode::Up, KeyModifiers::NONE), &reg);
    assert!(
        !matches!(
            up,
            InputOutcome::Action(Action::DashboardSelectNext | Action::DashboardSelectPrev)
        ),
        "Up with dispatch content must move the caret, not the list, got {up:?}",
    );
}

/// Space closes the peek when the reply is empty (quick toggle) but
/// types a space once the user has started composing.
#[test]
fn peek_space_types_into_reply() {
    let mut state = state_with_open_peek();
    let reg = crate::actions::ActionRegistry::defaults();
    // The peek is tied to selection now, so Space is plain text (no
    // close) — Esc unselects instead.
    for c in ['h', 'i'] {
        let _ = state.handle_key(&KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE), &reg);
    }
    let space = KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE);
    let _ = state.handle_key(&space, &reg);
    let _ = state.handle_key(&KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE), &reg);
    assert_eq!(state.peek_reply.text(), "hi y");
    // Peek stays open while the row is selected.
    assert!(state.peek.is_some());
}

/// Esc unselects: with an empty draft it clears the selection and
/// focuses the `[+ New Agent]` button (the new-session entry); a
/// typed draft is cleared first.
#[test]
fn peek_esc_clears_draft_then_unselects() {
    let mut state = state_with_open_peek();
    let reg = crate::actions::ActionRegistry::defaults();
    state.peek_reply.set_text("draft");
    let esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
    // First Esc clears the draft, keeps the peek + selection.
    let _ = state.handle_key(&esc, &reg);
    assert!(state.peek_reply.text().is_empty());
    assert!(state.selected.is_some());
    // Second Esc unselects → focuses the + New Agent button + closes peek.
    let _ = state.handle_key(&esc, &reg);
    assert!(state.peek.is_none());
    assert!(state.selected.is_none());
    assert!(state.new_agent_button_focused);
}

/// Ctrl-modified chords are never TYPED into the reply: non-bound
/// editing chords (Ctrl+A → caret-to-start) are delegated to the
/// widget as edits, while registry-bound dashboard chords (Ctrl+X
/// stop, Ctrl+T pin, …) fall through so they keep firing with the
/// peek open.
#[test]
fn peek_ctrl_keys_fall_through_not_typed() {
    use crate::app::actions::Action;
    let mut state = state_with_open_peek();
    let reg = crate::actions::ActionRegistry::defaults();
    let ctrl_a = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL);
    let _ = state.handle_key(&ctrl_a, &reg);
    // 'a' must not have been typed into the reply (Ctrl+A is the
    // caret-to-start editing chord, not text input).
    assert!(state.peek_reply.text().is_empty());

    // A registry-bound dashboard chord still falls through to its
    // action with the peek open (Ctrl+T → pin toggle).
    let ctrl_t = KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL);
    assert!(
        matches!(
            state.handle_key(&ctrl_t, &reg),
            InputOutcome::Action(Action::DashboardTogglePin)
        ),
        "registry-bound Ctrl+T must keep firing with the peek open",
    );
    assert!(state.peek_reply.text().is_empty());
}

/// Ctrl+C / Ctrl+D bubble up as `Unchanged` so the app-global
/// quit handler fires — they are not typed into the reply or
/// swallowed by the peek.
#[test]
fn peek_ctrl_c_d_bubble_to_global_quit() {
    let mut state = state_with_open_peek();
    let reg = crate::actions::ActionRegistry::defaults();
    let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
    assert!(matches!(
        state.handle_key(&ctrl_c, &reg),
        InputOutcome::Unchanged
    ));
    let ctrl_d = KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL);
    assert!(matches!(
        state.handle_key(&ctrl_d, &reg),
        InputOutcome::Unchanged
    ));
    // The peek stays open and nothing was typed.
    assert!(state.peek.is_some());
    assert!(state.peek_reply.text().is_empty());
}

/// Question picker flow: no option is selected by default (arrows switch
/// agents, Enter opens). A number key selects (and toggles) an option;
/// then `↑`/`↓` move within the options (spilling to the prev/next agent
/// at the edges) and `Enter` answers the selected option.
#[test]
fn peek_arrows_navigate_options_and_enter_answers() {
    use crate::app::actions::Action;
    let mut state = make_state_with_selection();
    let mut f = peek_fields_for_test("Awaiting your input");
    f.question = Some("Allow Edit?".into());
    f.options = vec![
        ("allow".into(), "Allow".into()),
        ("deny".into(), "Deny".into()),
    ];
    f.request_id = Some(7);
    state.peek = Some(super::super::peek::PeekPanelState::new(
        DashboardRowId::TopLevel(AgentId(0)),
        f,
    ));
    let reg = crate::actions::ActionRegistry::defaults();

    // Default: nothing selected → Down switches to the next agent.
    let down = KeyEvent::new(KeyCode::Down, KeyModifiers::NONE);
    assert!(matches!(
        state.handle_key(&down, &reg),
        InputOutcome::Action(Action::DashboardSelectNext)
    ));
    assert_eq!(state.peek.as_ref().unwrap().selected_option, None);

    // Pressing `1` selects the first option (and focuses the picker).
    let one = KeyEvent::new(KeyCode::Char('1'), KeyModifiers::NONE);
    assert!(matches!(
        state.handle_key(&one, &reg),
        InputOutcome::Changed
    ));
    assert_eq!(state.peek.as_ref().unwrap().selected_option, Some(0));

    // Now Down moves within the options to option 1.
    assert!(matches!(
        state.handle_key(&down, &reg),
        InputOutcome::Changed
    ));
    assert_eq!(state.peek.as_ref().unwrap().selected_option, Some(1));
    // Down again at the LAST option spills out to the next agent; the
    // selection is left unchanged.
    assert!(matches!(
        state.handle_key(&down, &reg),
        InputOutcome::Action(Action::DashboardSelectNext)
    ));
    assert_eq!(state.peek.as_ref().unwrap().selected_option, Some(1));

    // Enter answers the selected option (index 1 → "deny").
    let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
    match state.handle_key(&enter, &reg) {
        InputOutcome::Action(Action::DashboardPermissionSelect {
            request_id,
            option_id,
            ..
        }) => {
            assert_eq!(request_id, 7);
            assert_eq!(option_id.0.as_ref(), "deny");
        }
        other => panic!("expected DashboardPermissionSelect, got {other:?}"),
    }

    // Up moves back toward the first option.
    let up = KeyEvent::new(KeyCode::Up, KeyModifiers::NONE);
    let _ = state.handle_key(&up, &reg);
    assert_eq!(state.peek.as_ref().unwrap().selected_option, Some(0));
    // Up again at the FIRST option spills out to the previous row.
    assert!(matches!(
        state.handle_key(&up, &reg),
        InputOutcome::Action(Action::DashboardSelectPrev)
    ));
    assert_eq!(state.peek.as_ref().unwrap().selected_option, Some(0));

    // Pressing `1` again toggles the selection off → back to navigation,
    // where Enter opens the agent in detail.
    assert!(matches!(
        state.handle_key(&one, &reg),
        InputOutcome::Changed
    ));
    assert_eq!(state.peek.as_ref().unwrap().selected_option, None);
    assert!(matches!(
        state.handle_key(&enter, &reg),
        InputOutcome::Action(Action::DashboardAttach(_))
    ));
}

/// Right mirrors Enter on the question-picker navigation surface:
/// with the panel focused and NO option selected, a bare Right opens
/// the peeked row in detail — just like Enter. Regression guard: the
/// `question_mode` block ends in a modal catch-all that returns
/// `Unchanged`, so without an explicit Right arm Enter opened but
/// Right did nothing (an inconsistent dead key).
#[test]
fn peek_right_arrow_opens_agent_in_focused_question_picker() {
    use crate::app::actions::Action;
    let mut state = make_state_with_selection();
    let mut f = peek_fields_for_test("Awaiting your input");
    f.question = Some("Allow Edit?".into());
    f.options = vec![
        ("allow".into(), "Allow".into()),
        ("deny".into(), "Deny".into()),
    ];
    f.request_id = Some(7);
    state.peek = Some(super::super::peek::PeekPanelState::new(
        DashboardRowId::TopLevel(AgentId(0)),
        f,
    ));
    // Pin focused: PeekPanelState::new seeds from load_vim_mode().
    state.peek.as_mut().unwrap().focused = true;
    let reg = crate::actions::ActionRegistry::defaults();
    assert_eq!(state.peek.as_ref().unwrap().selected_option, None);

    let right = KeyEvent::new(KeyCode::Right, KeyModifiers::NONE);
    match state.handle_key(&right, &reg) {
        InputOutcome::Action(Action::DashboardAttach(row)) => {
            assert_eq!(row, DashboardRowId::TopLevel(AgentId(0)));
        }
        other => panic!(
            "Right in the focused question picker must open the agent (DashboardAttach), got {other:?}",
        ),
    }
}

/// The reject ("No") option accepts inline free-text feedback:
/// typing on it composes a message and `Enter` sends the rejection
/// with that feedback. Typing on a non-reject option is consumed.
#[test]
fn peek_reject_option_accepts_typed_feedback() {
    use crate::app::actions::Action;
    let mut state = make_state_with_selection();
    let mut f = peek_fields_for_test("Awaiting your input");
    f.question = Some("Allow Edit?".into());
    f.options = vec![
        ("allow".into(), "Allow".into()),
        ("reject".into(), "No".into()),
    ];
    f.request_id = Some(9);
    f.reject_option = Some(1);
    state.peek = Some(super::super::peek::PeekPanelState::new(
        DashboardRowId::TopLevel(AgentId(0)),
        f,
    ));
    let reg = crate::actions::ActionRegistry::defaults();

    // With no option selected, typing a letter is consumed — no feedback
    // composed and it doesn't leak into the reply buffer.
    let _ = state.handle_key(&KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE), &reg);
    assert!(state.peek_reply.text().is_empty());

    // Select the reject option (index 1 → key `2`), then type feedback.
    let _ = state.handle_key(&KeyEvent::new(KeyCode::Char('2'), KeyModifiers::NONE), &reg);
    assert_eq!(state.peek.as_ref().unwrap().selected_option, Some(1));
    for c in ['n', 'o', 'p', 'e'] {
        let _ = state.handle_key(&KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE), &reg);
    }
    assert_eq!(state.peek_reply.text(), "nope");

    // Enter sends the rejection with the typed feedback.
    match state.handle_key(&KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &reg) {
        InputOutcome::Action(Action::DashboardPermissionFollowup {
            request_id, text, ..
        }) => {
            assert_eq!(request_id, 9);
            assert_eq!(text, "nope");
        }
        other => panic!("expected DashboardPermissionFollowup, got {other:?}"),
    }
}

/// `clear_peek_reply` (used on every lifecycle clear — row change,
/// open/close, send) wipes the undo history too, so `Ctrl+Z` can't
/// resurrect a draft typed for a DIFFERENT agent onto the newly
/// peeked one. Regression for the cross-agent mis-send hole that a
/// bare `set_text("")` left open (set_text records an undoable
/// `Replace` checkpoint).
#[test]
fn peek_clear_wipes_undo_so_ctrl_z_cannot_resurrect_draft() {
    let mut state = state_with_open_peek();
    let reg = crate::actions::ActionRegistry::defaults();
    for c in "secret for A".chars() {
        let _ = state.handle_key(&KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE), &reg);
    }
    assert_eq!(state.peek_reply.text(), "secret for A");
    // Simulate the row-change / lifecycle clear.
    state.clear_peek_reply();
    assert!(state.peek_reply.text().is_empty());
    // Ctrl+Z while the (now different-agent) reply is focused must
    // NOT bring the old draft back.
    let _ = state.handle_key(
        &KeyEvent::new(KeyCode::Char('z'), KeyModifiers::CONTROL),
        &reg,
    );
    assert!(
        state.peek_reply.text().is_empty(),
        "undo must not resurrect a cleared cross-agent draft, got {:?}",
        state.peek_reply.text(),
    );
}

/// Typing `@` in the peek reply activates the session-less file
/// context picker (rooted at the launch cwd, like the dispatch box),
/// so the `@` dropdown can stream in and render above the panel.
#[test]
fn peek_typing_at_activates_file_search() {
    let mut state = state_with_open_peek();
    let reg = crate::actions::ActionRegistry::defaults();
    assert!(
        state.peek_reply.file_search.context().is_none(),
        "no @-context before typing @",
    );
    for c in ['@', 's', 'r', 'c'] {
        let _ = state.handle_key(&KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE), &reg);
    }
    assert_eq!(state.peek_reply.text(), "@src");
    assert!(
        state.peek_reply.file_search.context().is_some(),
        "typing @ must activate the reply's file-search picker",
    );
}

/// Mouse-wheel scrolling over the `@` dropdown must drive the SAME
/// picker that is rendered: the peek reply's while the panel is
/// open, the dispatch box's otherwise. (Regression: the wheel
/// intercept hardcoded `dispatch.file_search`, so scrolling the peek
/// dropdown moved the hidden dispatch selection while the visible
/// list stayed put.) Uses `context()` as a cheap observable for
/// "which picker" — `@`-context is set synchronously, unlike the
/// async results `is_visible()` needs.
#[test]
fn dropdown_file_search_follows_peek_state() {
    // Peek open → the picker behind the dropdown is the reply's.
    let mut open = state_with_open_peek();
    open.peek_reply.file_search.update_context("@a", 2);
    assert!(
        open.dropdown_file_search_mut().context().is_some(),
        "with the peek open, wheel scrolling must target the reply's picker",
    );

    // Peek closed → it's the dispatch box's.
    let mut closed = DashboardState::new();
    closed.dispatch.file_search.update_context("@b", 2);
    assert!(closed.peek.is_none());
    assert!(
        closed.dropdown_file_search_mut().context().is_some(),
        "with the peek closed, wheel scrolling must target the dispatch picker",
    );
}

/// The reply's `@` picker roots LAZILY at the peeked agent's cwd: a
/// bare cursor move (navigation) never retargets the daemon (no
/// thread churn), and the retarget lands on the first composing
/// keystroke, deduped so a same-cwd agent switch is free.
#[test]
fn peek_reply_file_search_retargets_lazily_on_compose() {
    let mut state = state_with_open_peek();
    let reg = crate::actions::ActionRegistry::defaults();
    // The render pass records the peeked agent's cwd; simulate it.
    state.set_peek_reply_target_cwd(Some(PathBuf::from("/work/repo-a")));
    assert_eq!(state.peek_reply_cwd, None, "daemon not retargeted yet");

    // Bare Down on an EMPTY reply switches agents — must NOT retarget.
    let _ = state.handle_key(&KeyEvent::new(KeyCode::Down, KeyModifiers::NONE), &reg);
    assert_eq!(
        state.peek_reply_cwd, None,
        "navigation must not spawn a matcher daemon",
    );

    // First composing keystroke retargets to the recorded cwd.
    let _ = state.handle_key(&KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE), &reg);
    assert_eq!(
        state.peek_reply_cwd.as_deref(),
        Some(Path::new("/work/repo-a")),
        "first compose must root the picker at the peeked agent's cwd",
    );

    // Switching to a different-cwd agent retargets once on next compose.
    state.set_peek_reply_target_cwd(Some(PathBuf::from("/work/repo-b")));
    let _ = state.handle_key(&KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE), &reg);
    assert_eq!(
        state.peek_reply_cwd.as_deref(),
        Some(Path::new("/work/repo-b")),
        "a cwd change retargets on the next compose",
    );
}

/// In question mode the `❯ reply` line is hidden, so a paste must NOT
/// silently fill the (invisible) reply buffer unless the reject /
/// "Other" free-text option is the selected one. (Regression: paste
/// used to land in `peek_reply` regardless and resurface later.)
#[test]
fn peek_paste_in_question_mode_gated_on_reject_selection() {
    let mut state = make_state_with_selection();
    let reg = crate::actions::ActionRegistry::defaults();
    let mut f = peek_fields_for_test("Awaiting your input");
    f.question = Some("Allow Edit?".into());
    f.options = vec![
        ("allow".into(), "Allow".into()),
        ("reject".into(), "No".into()),
    ];
    f.request_id = Some(9);
    f.reject_option = Some(1);
    state.peek = Some(super::super::peek::PeekPanelState::new(
        DashboardRowId::TopLevel(AgentId(0)),
        f,
    ));

    // No option selected → paste is dropped (would be invisible).
    let outcome = state.handle_input(&Event::Paste("ignored".to_string()), &reg);
    assert!(matches!(outcome, InputOutcome::Unchanged));
    assert!(
        state.peek_reply.text().is_empty(),
        "paste must not fill the hidden reply when no reject option is selected",
    );

    // Select the reject option → paste now lands in the feedback field.
    let _ = state.handle_key(&KeyEvent::new(KeyCode::Char('2'), KeyModifiers::NONE), &reg);
    assert_eq!(state.peek.as_ref().unwrap().selected_option, Some(1));
    let outcome = state.handle_input(&Event::Paste("real feedback".to_string()), &reg);
    assert!(matches!(outcome, InputOutcome::Changed));
    assert_eq!(state.peek_reply.text(), "real feedback");
}

/// The Ask tool (`AskUserQuestion`) is answered from the peek too:
/// selecting an option emits `DashboardQuestionAnswer { option_idx }`,
/// and the "Other" free-text row emits it with `option_idx: None` +
/// the typed text. (Ask questions carry no `request_id`.)
#[test]
fn peek_ask_question_answer_routing() {
    use crate::app::actions::Action;
    let mut state = make_state_with_selection();
    let mut f = peek_fields_for_test("Awaiting your input");
    f.question = Some("Which approach?".into());
    // Two real options + an appended "Other" free-text row.
    f.options = vec![
        ("Redis".into(), "Redis".into()),
        ("In-memory".into(), "In-memory".into()),
        ("__other__".into(), "Other".into()),
    ];
    f.reject_option = Some(2);
    f.request_id = None; // ← marks it as an ask question, not a permission
    state.peek = Some(super::super::peek::PeekPanelState::new(
        DashboardRowId::TopLevel(AgentId(0)),
        f,
    ));
    let reg = crate::actions::ActionRegistry::defaults();

    // Select the first option (key `1`), then Enter answers by index.
    let _ = state.handle_key(&KeyEvent::new(KeyCode::Char('1'), KeyModifiers::NONE), &reg);
    assert_eq!(state.peek.as_ref().unwrap().selected_option, Some(0));
    let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
    match state.handle_key(&enter, &reg) {
        InputOutcome::Action(Action::DashboardQuestionAnswer {
            option_idx,
            freeform,
            ..
        }) => {
            assert_eq!(option_idx, Some(0));
            assert!(freeform.is_empty());
        }
        other => panic!("expected DashboardQuestionAnswer, got {other:?}"),
    }

    // Select the "Other" row (index 2 → key `3`), type free-text, Enter
    // → freeform answer.
    let _ = state.handle_key(&KeyEvent::new(KeyCode::Char('3'), KeyModifiers::NONE), &reg);
    assert_eq!(state.peek.as_ref().unwrap().selected_option, Some(2));
    for c in ['s', 'q', 'l'] {
        let _ = state.handle_key(&KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE), &reg);
    }
    match state.handle_key(&enter, &reg) {
        InputOutcome::Action(Action::DashboardQuestionAnswer {
            option_idx,
            freeform,
            ..
        }) => {
            assert_eq!(option_idx, None);
            assert_eq!(freeform, "sql");
        }
        other => panic!("expected DashboardQuestionAnswer(Other), got {other:?}"),
    }
}

/// Tab toggles peek reply focus; unfocused printable re-focuses and types (non-vim).
#[test]
fn peek_tab_toggles_focus_and_typing_refocuses() {
    crate::appearance::cache::set_vim_mode(false);
    let mut state = state_with_open_peek();
    let reg = crate::actions::ActionRegistry::defaults();
    assert!(state.peek.as_ref().unwrap().focused);
    let tab = KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE);
    let _ = state.handle_key(&tab, &reg);
    assert!(!state.peek.as_ref().unwrap().focused, "Tab must unfocus");
    // Typing while unfocused re-focuses and composes.
    let _ = state.handle_key(&KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE), &reg);
    assert!(
        state.peek.as_ref().unwrap().focused,
        "typing must re-focus the reply"
    );
    assert_eq!(state.peek_reply.text(), "y");
}

/// Vim: peek reply starts unfocused; j navigates; Enter focuses (no attach).
#[test]
fn vim_peek_opens_unfocused_jk_nav_enter_focuses() {
    let mut state = state_with_open_peek();
    // Fixture pins vim off; re-enable and rebuild so the panel is
    // born unfocused under vim.
    crate::appearance::cache::set_vim_mode(true);
    state.peek = Some(super::super::peek::PeekPanelState::new(
        DashboardRowId::TopLevel(AgentId(0)),
        peek_fields_for_test("Idle"),
    ));
    let reg = crate::actions::ActionRegistry::defaults();
    assert!(
        !state.peek.as_ref().unwrap().focused,
        "vim peek must not auto-focus the reply"
    );
    let j = state.handle_key(&KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE), &reg);
    assert!(
        matches!(j, InputOutcome::Action(Action::DashboardSelectNext)),
        "vim j on unfocused peek must navigate, got {j:?}"
    );
    assert!(
        state.peek_reply.text().is_empty(),
        "j must not type into the reply, got {:?}",
        state.peek_reply.text()
    );
    let enter = state.handle_key(&KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &reg);
    assert!(matches!(enter, InputOutcome::Changed));
    assert!(
        state.peek.as_ref().unwrap().focused,
        "Enter must focus the peek reply in vim mode"
    );
    let _ = state.handle_key(&KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE), &reg);
    assert_eq!(state.peek_reply.text(), "j");
    crate::appearance::cache::set_vim_mode(false);
}

/// Vim unfocused: `i` focuses without inserting; other printables are swallowed.
#[test]
fn vim_peek_unfocused_i_focuses_printable_swallowed() {
    let mut state = state_with_open_peek();
    crate::appearance::cache::set_vim_mode(true);
    state.peek = Some(super::super::peek::PeekPanelState::new(
        DashboardRowId::TopLevel(AgentId(0)),
        peek_fields_for_test("Idle"),
    ));
    let reg = crate::actions::ActionRegistry::defaults();
    assert!(!state.peek.as_ref().unwrap().focused);

    let x = state.handle_key(&KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE), &reg);
    assert!(
        matches!(x, InputOutcome::Unchanged),
        "vim unfocused printable must be swallowed, got {x:?}"
    );
    assert!(
        !state.peek.as_ref().unwrap().focused,
        "swallowed key must not focus the reply"
    );
    assert!(
        state.peek_reply.text().is_empty(),
        "swallowed key must not type, got {:?}",
        state.peek_reply.text()
    );

    let i = state.handle_key(&KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE), &reg);
    assert!(matches!(i, InputOutcome::Changed));
    assert!(
        state.peek.as_ref().unwrap().focused,
        "i must focus the peek reply"
    );
    assert!(
        state.peek_reply.text().is_empty(),
        "i must not be inserted, got {:?}",
        state.peek_reply.text()
    );
    crate::appearance::cache::set_vim_mode(false);
}

/// Vim: apply_fields row change clears peek reply focus.
#[test]
fn vim_peek_row_change_unfocuses_reply() {
    let mut state = state_with_open_peek();
    crate::appearance::cache::set_vim_mode(true);
    state.peek.as_mut().unwrap().focused = true;
    let other = DashboardRowId::TopLevel(AgentId(99));
    let fields = super::super::peek::PeekFields {
        label: "other".into(),
        time_ago: String::new(),
        response_type: "Idle".into(),
        last_user_message: None,
        question: None,
        options: vec![],
        request_id: None,
        reject_option: None,
    };
    let changed = state.peek.as_mut().unwrap().apply_fields(other, fields);
    assert!(changed);
    assert!(
        !state.peek.as_ref().unwrap().focused,
        "vim row change must unfocus the reply"
    );
    crate::appearance::cache::set_vim_mode(false);
}

/// Non-registry editing chords reach the reply widget while the
/// peek is focused: Ctrl+A moves the caret to the start and Ctrl+K
/// kills to end-of-line — the full `PromptWidget` editing surface,
/// not the old bare-char-only editor.
#[test]
fn peek_editing_chords_reach_reply_widget() {
    let mut state = state_with_open_peek();
    let reg = crate::actions::ActionRegistry::defaults();
    for c in ['a', 'b', 'c'] {
        let _ = state.handle_key(&KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE), &reg);
    }
    assert_eq!(state.peek_reply.text(), "abc");
    // Ctrl+A → caret to line start (consumed by the widget, NOT a
    // dashboard action and NOT typed).
    let _ = state.handle_key(
        &KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL),
        &reg,
    );
    assert_eq!(state.peek_reply.cursor(), 0, "Ctrl+A must move the caret");
    // Ctrl+K → kill to end of line.
    let _ = state.handle_key(
        &KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL),
        &reg,
    );
    assert!(
        state.peek_reply.text().is_empty(),
        "Ctrl+K must kill to end of line, got {:?}",
        state.peek_reply.text(),
    );
    // The hidden dispatch input was never touched.
    assert!(state.dispatch.text().is_empty());
}

/// Drag-selecting text in the dispatch box works like the peek
/// reply: Down inside the rect anchors the drag, Drag extends the
/// textarea selection, and Up finishes it — Drag/Up are forwarded
/// even when the pointer leaves the box.
#[test]
fn dispatch_mouse_drag_selects_text() {
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    use ratatui::buffer::Buffer;

    let style = crate::views::prompt_widget::PromptStyle {
        focused: true,
        show_prefix: true,
        vpad_top: 0,
        chrome: false,
        ..crate::views::prompt_widget::PromptStyle::default()
    };
    let mut state = DashboardState::new();
    state.dispatch.set_text("hello world");
    let rect = Rect::new(2, 1, 60, 1);
    state.dispatch_rect = Some(rect);
    let mut buf = Buffer::empty(Rect::new(0, 0, 80, 10));
    let _ = state
        .dispatch
        .draw(&mut buf, rect, None, &style, None, None);
    for (kind, column) in [
        (MouseEventKind::Down(MouseButton::Left), 4),
        (MouseEventKind::Drag(MouseButton::Left), 12),
        // The drag continues past the right edge of the box and is
        // released there; the selection must keep extending.
        (MouseEventKind::Drag(MouseButton::Left), 70),
        (MouseEventKind::Up(MouseButton::Left), 70),
    ] {
        let _ = state.handle_mouse(&MouseEvent {
            kind,
            column,
            row: 1,
            modifiers: KeyModifiers::NONE,
        });
    }
    assert_eq!(
        state.dispatch.textarea.selection_range(),
        Some(0..11),
        "dispatch drag must extend the textarea selection like the peek reply",
    );
}

/// A left click inside the recorded reply rect focuses the reply
/// input (mirrors the dispatch box's click-to-focus) and routes the
/// event to the widget.
#[test]
fn peek_mouse_click_on_reply_rect_focuses() {
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    let mut state = state_with_open_peek();
    state.peek.as_mut().unwrap().focused = false;
    state.peek_reply_rect = Some(Rect::new(2, 10, 40, 1));
    let click = MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 5,
        row: 10,
        modifiers: KeyModifiers::NONE,
    };
    let outcome = state.handle_mouse(&click);
    assert!(matches!(outcome, InputOutcome::Changed));
    assert!(
        state.peek.as_ref().unwrap().focused,
        "a click on the reply rect must focus the reply input",
    );
    // A click outside the rect (on no row) leaves focus alone.
    state.peek.as_mut().unwrap().focused = false;
    let miss = MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 70,
        row: 20,
        modifiers: KeyModifiers::NONE,
    };
    let _ = state.handle_mouse(&miss);
    assert!(
        !state.peek.as_ref().unwrap().focused,
        "a click outside the reply rect must not grab focus",
    );
}

/// Esc never wipes a typed dispatch draft. On a focused input the
/// first Esc unfocuses (blurs) to the overview list so the user can
/// navigate; the draft is left intact. A later Esc exits, still
/// keeping the draft (retained across a same-process close/reopen of
/// the dashboard; not persisted across an app restart).
#[test]
fn esc_preserves_dispatch_text() {
    let mut state = DashboardState::new();
    state.dispatch.set_text("fix the bug");
    assert!(!state.list_focused, "input focused by default");
    let reg = crate::actions::ActionRegistry::defaults();
    let key = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
    // First Esc blurs the input → list focus; draft preserved.
    let first = state.handle_key(&key, &reg);
    assert!(matches!(first, InputOutcome::Changed));
    assert!(state.list_focused, "Esc unfocuses the input");
    assert_eq!(state.dispatch.text(), "fix the bug");
    // Second Esc (list focused, nothing selected) exits — draft kept.
    let second = state.handle_key(&key, &reg);
    assert!(matches!(
        second,
        InputOutcome::Action(Action::ExitDashboard)
    ));
    assert_eq!(state.dispatch.text(), "fix the bug");
}

/// Esc on a focused input blurs to the list without touching the
/// draft, even when a row is selected (the selection survives the
/// blur and is only backed out of by a later Esc).
#[test]
fn esc_blurs_input_keeps_draft_and_selection() {
    let mut state = make_state_with_selection();
    state.dispatch.set_text("hello");
    assert!(!state.list_focused, "input focused by default");
    let reg = crate::actions::ActionRegistry::defaults();
    let key = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
    let outcome = state.handle_key(&key, &reg);
    assert!(matches!(outcome, InputOutcome::Changed));
    assert!(state.list_focused, "Esc unfocuses the input");
    assert!(state.selected.is_some(), "selection preserved on blur");
    assert_eq!(state.dispatch.text(), "hello", "draft is preserved");
}

/// edge case 13: Esc with empty input + active filter
/// clears filter.
#[test]
fn esc_clears_active_filter() {
    let mut state = make_state_with_selection();
    state.filter = Filter::Agent("reviewer".into());
    let reg = crate::actions::ActionRegistry::defaults();
    let key = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
    let outcome = state.handle_key(&key, &reg);
    assert!(matches!(outcome, InputOutcome::Changed));
    assert!(matches!(state.filter, Filter::None));
}

/// Esc with nothing to back out of: the first Esc unfocuses the
/// input (→ overview list), the second exits the dashboard. The
/// focus tier sits above exit so a focused input always blurs first.
#[test]
fn esc_with_nothing_to_clear_blurs_then_exits() {
    let mut state = DashboardState::new();
    assert!(!state.list_focused, "input focused by default");
    let reg = crate::actions::ActionRegistry::defaults();
    let key = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
    let first = state.handle_key(&key, &reg);
    assert!(matches!(first, InputOutcome::Changed));
    assert!(state.list_focused, "first Esc unfocuses the input");
    let second = state.handle_key(&key, &reg);
    assert!(matches!(
        second,
        InputOutcome::Action(Action::ExitDashboard)
    ));
}

/// With the list focused and a row selected, Esc DESELECTS instead
/// of exiting. The user's contract hinges on this: a selected row
/// turns the dispatch input into "reply to this agent"; deselecting
/// flips it back to "create a new session" without leaving the
/// dashboard. (From a focused input Esc would blur first; here we
/// start already on the list.)
#[test]
fn esc_with_selection_deselects() {
    let mut state = make_state_with_selection();
    state.list_focused = true;
    let reg = crate::actions::ActionRegistry::defaults();
    let key = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
    let outcome = state.handle_key(&key, &reg);
    assert!(
        matches!(outcome, InputOutcome::Changed),
        "Esc on a selected dashboard must report `Changed`, got {outcome:?}",
    );
    assert!(
        state.selected.is_none(),
        "Esc must clear `selected` so the next dispatch reaches the new-session path",
    );
    assert!(
        state.new_agent_button_focused,
        "Esc-deselect must focus the `[+ New Agent]` button as the new cursor target",
    );
}

/// Enter with an empty prompt while the button is
/// focused emits `DashboardCreateNewAgentWithDetail`. The
/// state handler returns the action; the dispatcher then
/// spawns the session + switches to its detail view.
#[test]
fn enter_on_focused_button_with_empty_prompt_emits_create_with_detail() {
    use crate::app::actions::Action;
    let mut state = DashboardState::new();
    // Fresh state defaults to button-focused; pin that
    // precondition so a future regression doesn't quietly
    // flip the default away from the button.
    assert!(state.new_agent_button_focused);
    let reg = crate::actions::ActionRegistry::defaults();
    let key = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
    let outcome = state.handle_key(&key, &reg);
    assert!(
        matches!(
            outcome,
            InputOutcome::Action(Action::DashboardCreateNewAgentWithDetail),
        ),
        "Enter on focused button with empty prompt must emit \
         DashboardCreateNewAgentWithDetail, got: {outcome:?}",
    );
}

/// Enter with a NON-empty prompt while the button is
/// focused emits `DashboardDispatch` (the regular new-session
/// path). Detail view does NOT open: the user wanted to fire
/// off a session and keep working in the dashboard.
#[test]
fn enter_on_focused_button_with_non_empty_prompt_emits_dispatch() {
    use crate::app::actions::Action;
    let mut state = DashboardState::new();
    state.dispatch.set_text("queue a task");
    let reg = crate::actions::ActionRegistry::defaults();
    let key = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
    let outcome = state.handle_key(&key, &reg);
    match outcome {
        InputOutcome::Action(Action::DashboardDispatch { text, attach }) => {
            assert_eq!(text, "queue a task");
            assert!(!attach, "plain Enter must keep `attach=false`");
        }
        other => panic!(
            "Enter on focused button with non-empty prompt must emit \
             DashboardDispatch (NOT CreateNewAgentWithDetail), got: {other:?}",
        ),
    }
}

/// Ctrl+S ("send + open") on focused button + non-empty prompt
/// emits `DashboardDispatch { attach: true }` so the
/// dispatcher's new-session arm switches view AND sets
/// `attached_agent`. The state handler doesn't know about attach
/// semantics — it just forwards the chord through the payload.
#[test]
fn ctrl_s_on_focused_button_with_text_emits_dispatch_with_attach_true() {
    use crate::app::actions::Action;
    let mut state = DashboardState::new();
    state.dispatch.set_text("send and open");
    let reg = crate::actions::ActionRegistry::defaults();
    let key = KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL);
    let outcome = state.handle_key(&key, &reg);
    match outcome {
        InputOutcome::Action(Action::DashboardDispatch { text, attach }) => {
            assert_eq!(text, "send and open");
            assert!(attach, "Ctrl+S must set `attach=true`");
        }
        other => panic!(
            "Ctrl+S on focused button with text must emit \
             DashboardDispatch {{ attach: true }}, got: {other:?}",
        ),
    }
}

/// Enter on a row-selected dashboard with an EMPTY
/// prompt emits `DashboardAttach(row_id)` so the dispatcher
/// opens the detail view without sending anything.
#[test]
fn enter_on_row_selected_empty_prompt_emits_attach() {
    use crate::app::actions::Action;
    let mut state = make_state_with_selection();
    let reg = crate::actions::ActionRegistry::defaults();
    let key = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
    let outcome = state.handle_key(&key, &reg);
    match outcome {
        InputOutcome::Action(Action::DashboardAttach(id)) => {
            assert_eq!(id, DashboardRowId::TopLevel(AgentId(0)));
        }
        other => panic!(
            "Enter on row + empty prompt must emit DashboardAttach, \
             got: {other:?}",
        ),
    }
}

/// Enter on a row-selected dashboard with TYPED text
/// emits `DashboardDispatch { attach: false }` so the
/// dispatcher's reply arm sends without leaving the
/// dashboard.
#[test]
fn enter_on_row_selected_with_text_emits_dispatch_no_attach() {
    use crate::app::actions::Action;
    let mut state = make_state_with_selection();
    state.dispatch.set_text("reply to selected");
    let reg = crate::actions::ActionRegistry::defaults();
    let key = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
    let outcome = state.handle_key(&key, &reg);
    match outcome {
        InputOutcome::Action(Action::DashboardDispatch { text, attach }) => {
            assert_eq!(text, "reply to selected");
            assert!(!attach);
        }
        other => panic!(
            "Enter on row + text must emit DashboardDispatch {{ attach: false }}, \
             got: {other:?}",
        ),
    }
}

/// Ctrl+S ("send + open") on a row-selected dashboard with TYPED
/// text emits `DashboardDispatch { attach: true }`.
#[test]
fn ctrl_s_on_row_selected_with_text_emits_dispatch_with_attach() {
    use crate::app::actions::Action;
    let mut state = make_state_with_selection();
    state.dispatch.set_text("reply and open");
    let reg = crate::actions::ActionRegistry::defaults();
    let key = KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL);
    let outcome = state.handle_key(&key, &reg);
    match outcome {
        InputOutcome::Action(Action::DashboardDispatch { text, attach }) => {
            assert_eq!(text, "reply and open");
            assert!(attach, "Ctrl+S must set `attach=true`");
        }
        other => panic!(
            "Ctrl+S on row + text must emit DashboardDispatch {{ attach: true }}, \
             got: {other:?}",
        ),
    }
}

/// Ctrl+S ("send + open") on focused button with EMPTY prompt
/// behaves like plain Enter — emits `CreateNewAgentWithDetail`.
/// There's nothing to "send" so the chord collapses to:
/// the only sensible interpretation is "create + open
/// detail", which the unmodified Enter already does.
#[test]
fn ctrl_s_on_focused_button_with_empty_prompt_emits_create_with_detail() {
    use crate::app::actions::Action;
    let mut state = DashboardState::new();
    assert!(state.new_agent_button_focused);
    let reg = crate::actions::ActionRegistry::defaults();
    let key = KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL);
    let outcome = state.handle_key(&key, &reg);
    assert!(
        matches!(
            outcome,
            InputOutcome::Action(Action::DashboardCreateNewAgentWithDetail),
        ),
        "Ctrl+S on focused button with empty prompt must collapse to \
         CreateNewAgentWithDetail, got: {outcome:?}",
    );
}

/// Full Esc cascade from a focused input with a row selected:
/// blur (→ list) → deselect (→ `[+ New Agent]`) → exit. Pins the
/// tier ordering, catching a regression that would skip any tier.
#[test]
fn esc_cascade_blurs_then_deselects_then_exits() {
    let mut state = make_state_with_selection();
    assert!(!state.list_focused, "input focused by default");
    let reg = crate::actions::ActionRegistry::defaults();
    let key = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
    // 1: blur the input to the list (selection survives).
    let first = state.handle_key(&key, &reg);
    assert!(matches!(first, InputOutcome::Changed));
    assert!(state.list_focused, "first Esc unfocuses the input");
    assert!(state.selected.is_some(), "selection survives the blur");
    // 2: deselect the row.
    let second = state.handle_key(&key, &reg);
    assert!(matches!(second, InputOutcome::Changed));
    assert!(state.selected.is_none());
    assert!(state.new_agent_button_focused);
    // 3: exit.
    let third = state.handle_key(&key, &reg);
    assert!(
        matches!(third, InputOutcome::Action(Action::ExitDashboard)),
        "third Esc must exit the dashboard, got {third:?}",
    );
}

/// New contract: a `a:` / `s:` / `#` prefix is NO LONGER treated
/// as a filter on Enter — filtering is the explicit `Ctrl+/`
/// search mode now, so a prompt that merely starts with a prefix
/// dispatches verbatim. This pins the bug fix: prefixed prompts
/// must not be silently swallowed as filters.
#[test]
fn enter_with_prefix_text_dispatches_not_filters() {
    let mut state = make_state_with_selection();
    state.dispatch.set_text("a:reviewer please refactor");
    let reg = crate::actions::ActionRegistry::defaults();
    let key = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
    let outcome = state.handle_key(&key, &reg);
    match outcome {
        InputOutcome::Action(Action::DashboardDispatch { text, attach }) => {
            assert_eq!(text, "a:reviewer please refactor");
            assert!(!attach);
        }
        other => panic!("prefix text must DISPATCH, not filter, got {other:?}"),
    }
    assert!(
        matches!(state.filter, Filter::None),
        "dispatch must not set a filter",
    );
}

/// edge case 21: Enter on free text dispatches.
/// Assert the payload matches the typed text and that
/// `attach` is false (no Shift modifier). A regression that
/// dispatched a different string (or swallowed the input) would
/// be invisible to a `matches!` assertion that ignores the
/// payload fields.
#[test]
fn enter_with_free_text_dispatches() {
    let mut state = make_state_with_selection();
    let typed = "write some tests";
    state.dispatch.set_text(typed);
    let reg = crate::actions::ActionRegistry::defaults();
    let key = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
    let outcome = state.handle_key(&key, &reg);
    match outcome {
        InputOutcome::Action(Action::DashboardDispatch { text, attach }) => {
            assert_eq!(text, typed, "Dispatch payload must echo the typed text");
            assert!(!attach, "Plain Enter must not set attach=true");
        }
        other => panic!("expected DashboardDispatch, got {other:?}"),
    }
}

// -----------------------------------------------------------------
// Reconciled dispatch-input features: slash commands, Alt+Enter
// multiline, vim-gated j/k, and paste — layered on top of the
// reply-mode / search-mode base.
// -----------------------------------------------------------------

/// A `/command` Enter routes through the session-less slash
/// dispatcher instead of becoming a new session's prompt.
#[test]
fn slash_command_on_enter_dispatches_slash() {
    let mut state = make_state_with_selection();
    state.dispatch.set_text("/help");
    let reg = crate::actions::ActionRegistry::defaults();
    let key = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
    match state.handle_key(&key, &reg) {
        InputOutcome::Action(Action::DashboardDispatchSlash { text }) => {
            assert_eq!(text, "/help");
        }
        other => panic!("expected DashboardDispatchSlash, got {other:?}"),
    }
}

/// Alt+Enter AND Shift+Enter insert a newline (multiline compose)
/// in the dispatch input rather than dispatching — "send + open"
/// moved to Ctrl+S so both Enter-modifier chords are free for
/// newlines (matching the agent prompt).
#[test]
fn alt_and_shift_enter_insert_newline_not_dispatch() {
    let reg = crate::actions::ActionRegistry::defaults();
    for modifier in [KeyModifiers::ALT, KeyModifiers::SHIFT] {
        let mut state = make_state_with_selection();
        for ch in "hi".chars() {
            let _ = state.handle_key(&KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE), &reg);
        }
        let outcome = state.handle_key(&KeyEvent::new(KeyCode::Enter, modifier), &reg);
        assert!(
            !matches!(outcome, InputOutcome::Action(_)),
            "{modifier:?}+Enter must not dispatch, got {outcome:?}"
        );
        assert_eq!(
            state.dispatch.text(),
            "hi\n",
            "{modifier:?}+Enter must insert a newline"
        );
    }
}

#[test]
fn compose_enter_is_newline_matrix() {
    // Strict swap: (multiline, mod_enter) → is_newline
    assert!(!compose_enter_is_newline(false, false));
    assert!(compose_enter_is_newline(false, true));
    assert!(compose_enter_is_newline(true, false));
    assert!(!compose_enter_is_newline(true, true));
}

/// With multiline_mode on, bare Enter inserts a newline (does not
/// dispatch) and Shift/Alt+Enter send — the agent-prompt swap.
#[test]
fn multiline_mode_swaps_enter_and_shift_enter() {
    use crate::app::actions::Action;
    let reg = crate::actions::ActionRegistry::defaults();
    let mut state = make_state_with_selection();
    state.multiline_mode = true;
    state.dispatch.set_text("line one");

    let bare = state.handle_key(&KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &reg);
    assert!(
        matches!(bare, InputOutcome::Changed),
        "bare Enter in multiline must insert newline, got {bare:?}"
    );
    assert!(
        state.dispatch.text().contains('\n'),
        "bare Enter must insert a newline, got {:?}",
        state.dispatch.text()
    );

    // After newline, Shift+Enter should dispatch the draft.
    let shift = state.handle_key(&KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT), &reg);
    match shift {
        InputOutcome::Action(Action::DashboardDispatch {
            text,
            attach: false,
        }) => {
            assert!(text.contains("line one"), "dispatch text: {text:?}");
        }
        other => panic!("Shift+Enter in multiline must dispatch, got {other:?}"),
    }
}

/// Multiline empty bare Enter inserts a newline (strict swap); Shift+Enter
/// open/create/attach.
#[test]
fn multiline_mode_empty_bare_enter_is_newline() {
    use crate::app::actions::Action;
    let reg = crate::actions::ActionRegistry::defaults();
    let mut state = make_state_with_selection();
    state.multiline_mode = true;
    state.dispatch.set_text("");
    let bare = state.handle_key(&KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &reg);
    assert!(
        matches!(bare, InputOutcome::Changed),
        "empty bare Enter in multiline must insert newline, got {bare:?}"
    );
    assert!(
        state.dispatch.text().contains('\n'),
        "got {:?}",
        state.dispatch.text()
    );

    state.dispatch.set_text("");
    match state.handle_key(&KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT), &reg) {
        InputOutcome::Action(Action::DashboardAttach(id)) => {
            assert_eq!(id, DashboardRowId::TopLevel(AgentId(0)));
        }
        other => panic!("empty Shift+Enter in multiline must attach, got {other:?}"),
    }
}

/// Ctrl+M toggles multiline via SetMultilineMode (same chord as agent).
#[test]
fn ctrl_m_toggles_multiline_mode() {
    use crate::app::actions::Action;
    let reg = crate::actions::ActionRegistry::defaults();
    let mut state = DashboardState::new();
    assert!(!state.multiline_mode);
    let outcome = state.handle_key(
        &KeyEvent::new(KeyCode::Char('m'), KeyModifiers::CONTROL),
        &reg,
    );
    match outcome {
        InputOutcome::Action(Action::SetMultilineMode(true)) => {}
        other => panic!("Ctrl+M must emit SetMultilineMode(true), got {other:?}"),
    }
    state.multiline_mode = true;
    let outcome = state.handle_key(
        &KeyEvent::new(KeyCode::Char('m'), KeyModifiers::CONTROL),
        &reg,
    );
    match outcome {
        InputOutcome::Action(Action::SetMultilineMode(false)) => {}
        other => panic!("Ctrl+M when on must emit SetMultilineMode(false), got {other:?}"),
    }
}

/// Bare `?` opens shortcuts when the draft is empty (input-focused) or
/// the list is focused; types into a non-empty draft.
#[test]
fn question_mark_honor_gate_matches_empty_or_list_focus() {
    let reg = crate::actions::ActionRegistry::defaults();
    let question = KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE);

    // Default: input-focused, empty → help.
    let mut state = DashboardState::new();
    assert!(!state.list_focused);
    assert!(matches!(
        state.handle_key(&question, &reg),
        InputOutcome::Action(Action::DashboardOpenShortcutsHelp)
    ));
    assert!(state.dispatch.text().is_empty());

    // Non-empty draft → type.
    state.dispatch.set_text("hello");
    let _ = state.handle_key(&question, &reg);
    assert!(
        state.dispatch.text().contains('?'),
        "non-empty draft: `?` must type, got {:?}",
        state.dispatch.text()
    );

    // List-focused with leftover draft → help, draft untouched.
    state.list_focused = true;
    state.dispatch.set_text("leftover");
    assert!(matches!(
        state.handle_key(&question, &reg),
        InputOutcome::Action(Action::DashboardOpenShortcutsHelp)
    ));
    assert_eq!(state.dispatch.text(), "leftover");

    // Empty peek reply → help; non-empty peek reply → type.
    let mut peek = state_with_open_peek();
    assert!(matches!(
        peek.handle_key(&question, &reg),
        InputOutcome::Action(Action::DashboardOpenShortcutsHelp)
    ));
    assert!(peek.peek_reply.text().is_empty());
    peek.peek_reply.set_text("draft");
    let _ = peek.handle_key(&question, &reg);
    assert!(
        peek.peek_reply.text().contains('?'),
        "non-empty peek reply: `?` must type, got {:?}",
        peek.peek_reply.text()
    );
}

/// vim-mode OFF — `j`/`k` type into the dispatch input (they are
/// NOT hijacked as row navigation). Mirrors the agent scrollback.
#[test]
fn vim_off_jk_type_into_input() {
    crate::appearance::cache::set_vim_mode(false);
    let reg = crate::actions::ActionRegistry::defaults();
    let mut state = DashboardState::new();
    for ch in ['j', 'k'] {
        let _ = state.handle_key(&KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE), &reg);
    }
    assert_eq!(
        state.dispatch.text(),
        "jk",
        "vim-off j/k must type into the input, not navigate"
    );
}

/// vim-mode ON + the overview list focused (via Tab) — `j`/`k`
/// navigate the row list. In the input focus they type (covered by
/// `vim_on_jk_type_into_input_when_focused`).
#[test]
fn vim_on_jk_navigate_when_list_focused() {
    crate::appearance::cache::set_vim_mode(true);
    let reg = crate::actions::ActionRegistry::defaults();
    let mut state = DashboardState::new();
    state.list_focused = true;
    let j = state.handle_key(&KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE), &reg);
    assert!(
        matches!(j, InputOutcome::Action(Action::DashboardSelectNext)),
        "vim j must select the next row, got {j:?}"
    );
    let k = state.handle_key(&KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE), &reg);
    assert!(
        matches!(k, InputOutcome::Action(Action::DashboardSelectPrev)),
        "vim k must select the previous row, got {k:?}"
    );
    crate::appearance::cache::set_vim_mode(false);
}

/// vim-mode ON but the INPUT focused (the default) — `j`/`k` type
/// into the dispatch prompt; navigation requires Tab to the overview
/// first. This is the "distinct focus areas" contract.
#[test]
fn vim_on_jk_type_into_input_when_focused() {
    crate::appearance::cache::set_vim_mode(true);
    let reg = crate::actions::ActionRegistry::defaults();
    let mut state = DashboardState::new();
    assert!(!state.list_focused, "input focused by default");
    for ch in ['j', 'k'] {
        let _ = state.handle_key(&KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE), &reg);
    }
    assert_eq!(
        state.dispatch.text(),
        "jk",
        "input-focused vim j/k must type, not navigate"
    );
    crate::appearance::cache::set_vim_mode(false);
}

/// Tab toggles the two-focus model: input bar ↔ overview list.
#[test]
fn tab_toggles_input_and_list_focus() {
    let reg = crate::actions::ActionRegistry::defaults();
    let mut state = make_state_with_selection();
    assert!(!state.list_focused, "input focused by default");
    let a = state.handle_key(&KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE), &reg);
    assert!(matches!(a, InputOutcome::Changed));
    assert!(state.list_focused, "Tab focuses the overview list");
    let b = state.handle_key(&KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE), &reg);
    assert!(matches!(b, InputOutcome::Changed));
    assert!(!state.list_focused, "Tab again returns focus to the input");
}

/// Shift+Tab emits `DashboardCycleMode` regardless of how the terminal
/// encodes it — `BackTab` (with or without a SHIFT modifier) or
/// `Tab`+SHIFT. Guards the regression where the registry's exact-modifier
/// `key!(BackTab)` lookup silently failed on `BackTab`+SHIFT.
#[test]
fn shift_tab_emits_cycle_mode_for_all_encodings() {
    let reg = crate::actions::ActionRegistry::defaults();
    for key in [
        KeyEvent::new(KeyCode::BackTab, KeyModifiers::NONE),
        KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT),
        KeyEvent::new(KeyCode::Tab, KeyModifiers::SHIFT),
    ] {
        let mut state = DashboardState::new();
        let outcome = state.handle_key(&key, &reg);
        assert!(
            matches!(outcome, InputOutcome::Action(Action::DashboardCycleMode)),
            "Shift+Tab ({key:?}) must emit DashboardCycleMode, got {outcome:?}",
        );
    }
}

/// Multiline must not treat Shift+Tab as the submit chord (is_mod_enter
/// requires KeyCode::Enter).
#[test]
fn multiline_shift_tab_cycles_mode_with_non_empty_draft() {
    let reg = crate::actions::ActionRegistry::defaults();
    for key in [
        KeyEvent::new(KeyCode::BackTab, KeyModifiers::NONE),
        KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT),
        KeyEvent::new(KeyCode::Tab, KeyModifiers::SHIFT),
    ] {
        let mut state = DashboardState::new();
        state.multiline_mode = true;
        state.dispatch.set_text("draft text");
        let outcome = state.handle_key(&key, &reg);
        assert!(
            matches!(outcome, InputOutcome::Action(Action::DashboardCycleMode)),
            "multiline + {key:?} must DashboardCycleMode, not send, got {outcome:?}",
        );
        assert_eq!(
            state.dispatch.text(),
            "draft text",
            "draft must not be consumed by Shift+Tab"
        );
    }
}

/// Shift+↑/↓ emits the reorder actions even with the peek open (the
/// default state when a row is selected) and regardless of focus. Guards
/// the reorder keybinding end-to-end through `handle_key`.
#[test]
fn shift_arrows_emit_reorder_with_peek_open() {
    let reg = crate::actions::ActionRegistry::defaults();
    for (code, expected) in [
        (KeyCode::Up, Action::DashboardReorderUp),
        (KeyCode::Down, Action::DashboardReorderDown),
    ] {
        let mut state = make_state_with_selection();
        // Peek is shown by default when a row is selected.
        state.peek = Some(super::super::peek::PeekPanelState::new(
            DashboardRowId::TopLevel(AgentId(0)),
            peek_fields_for_test("Idle"),
        ));
        let outcome = state.handle_key(&KeyEvent::new(code, KeyModifiers::SHIFT), &reg);
        assert!(
            matches!(&outcome, InputOutcome::Action(a) if std::mem::discriminant(a) == std::mem::discriminant(&expected)),
            "Shift+{code:?} must emit {expected:?}, got {outcome:?}",
        );
    }
}

/// Shift+Tab cycles the PEEKED agent's live mode while the peek is
/// open (emitting `DashboardPeekCycleMode`), but the new-session
/// staged mode (`DashboardCycleMode`) when no peek is shown.
#[test]
fn shift_tab_cycles_peeked_agent_mode_when_peek_open() {
    let reg = crate::actions::ActionRegistry::defaults();
    for key in [
        KeyEvent::new(KeyCode::BackTab, KeyModifiers::NONE),
        KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT),
        KeyEvent::new(KeyCode::Tab, KeyModifiers::SHIFT),
    ] {
        let mut state = make_state_with_selection();
        state.peek = Some(super::super::peek::PeekPanelState::new(
            DashboardRowId::TopLevel(AgentId(0)),
            peek_fields_for_test("Idle"),
        ));
        let outcome = state.handle_key(&key, &reg);
        assert!(
            matches!(
                outcome,
                InputOutcome::Action(Action::DashboardPeekCycleMode)
            ),
            "Shift+Tab ({key:?}) with peek open must emit DashboardPeekCycleMode, got {outcome:?}",
        );
    }
    // No peek → still the new-session staged-mode cycle.
    let mut state = DashboardState::new();
    let outcome = state.handle_key(&KeyEvent::new(KeyCode::BackTab, KeyModifiers::NONE), &reg);
    assert!(
        matches!(outcome, InputOutcome::Action(Action::DashboardCycleMode)),
        "Shift+Tab without peek must emit DashboardCycleMode, got {outcome:?}",
    );
}

/// Overview focused: Enter opens the focused row; Esc backs out of
/// the selection (focuses `[+ New Agent]`) and STAYS on the list —
/// it no longer returns to the input (Tab / `i` do that now).
#[test]
fn list_focus_enter_opens_and_esc_backs_out() {
    let reg = crate::actions::ActionRegistry::defaults();
    let mut state = make_state_with_selection();
    state.list_focused = true;
    let enter = state.handle_key(&KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &reg);
    assert!(
        matches!(enter, InputOutcome::Action(Action::DashboardAttach(_))),
        "Enter on the focused overview must open the selected row, got {enter:?}"
    );
    let esc = state.handle_key(&KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &reg);
    assert!(matches!(esc, InputOutcome::Changed));
    assert!(
        state.list_focused,
        "Esc stays on the list; Tab / i return focus to the input"
    );
    assert!(state.selected.is_none(), "Esc backs out of the selection");
    assert!(state.new_agent_button_focused);
}

/// Regression for the Esc-blur draft-loss path: with the list focused
/// (e.g. after Esc unfocuses the input) on the `[+ New Agent]`
/// button, Enter must SEND a typed draft rather than create an empty
/// session and silently drop it. An empty draft still
/// creates-with-detail.
#[test]
fn list_focus_enter_on_button_sends_draft_else_creates() {
    let reg = crate::actions::ActionRegistry::defaults();
    let key = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);

    // Draft present → dispatch it (no loss), staying on the dashboard.
    let mut state = DashboardState::new();
    assert!(state.new_agent_button_focused);
    state.dispatch.set_text("fix the bug");
    state.list_focused = true; // e.g. after an Esc blur
    match state.handle_key(&key, &reg) {
        InputOutcome::Action(Action::DashboardDispatch { text, attach }) => {
            assert_eq!(text, "fix the bug");
            assert!(!attach, "Enter sends + stays on the dashboard");
        }
        other => panic!("Enter with a draft must dispatch it, got {other:?}"),
    }

    // Empty draft → create-with-detail (unchanged behavior).
    let mut empty = DashboardState::new();
    empty.list_focused = true;
    assert!(matches!(
        empty.handle_key(&key, &reg),
        InputOutcome::Action(Action::DashboardCreateNewAgentWithDetail)
    ));
}

/// Overview focused: a non-nav printable key hands focus back to the
/// input. In vim mode `i` enters the input without typing the `i`.
#[test]
fn vim_i_returns_focus_to_input_without_typing() {
    crate::appearance::cache::set_vim_mode(true);
    let reg = crate::actions::ActionRegistry::defaults();
    let mut state = make_state_with_selection();
    state.list_focused = true;
    let outcome = state.handle_key(&KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE), &reg);
    assert!(matches!(outcome, InputOutcome::Changed));
    assert!(!state.list_focused, "i focuses the input");
    assert!(
        state.dispatch.text().is_empty(),
        "i must NOT be typed, got {:?}",
        state.dispatch.text()
    );
    crate::appearance::cache::set_vim_mode(false);
}

/// A multi-line bracketed paste keeps its full raw text (what gets
/// dispatched) while collapsing to a single chip element in the
/// textarea (rendered folded as `[Pasted: N lines]`).
#[test]
fn multiline_paste_folds_into_dispatch_input() {
    let mut state = DashboardState::new();
    let reg = crate::actions::ActionRegistry::defaults();
    let pasted = "line one\nline two\nline three\nline four";
    let _ = state.handle_input(&Event::Paste(pasted.to_string()), &reg);
    assert_eq!(state.dispatch.text(), pasted, "raw paste text is preserved");
    assert_eq!(
        state.dispatch.textarea.elements().len(),
        1,
        "multi-line paste must collapse to one chip element"
    );
}

/// Enter with the caret on a paste chip expands it instead of
/// dispatching (agent prompt parity).
#[test]
fn enter_on_dispatch_paste_chip_expands() {
    let mut state = DashboardState::new();
    let reg = crate::actions::ActionRegistry::defaults();
    let pasted = "line one\nline two\nline three\nline four";
    let _ = state.handle_input(&Event::Paste(pasted.to_string()), &reg);
    state.dispatch.set_cursor(0);
    let outcome = state.handle_key(&KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &reg);
    assert!(
        matches!(outcome, InputOutcome::Changed),
        "Enter on chip must expand, got {outcome:?}"
    );
    assert!(
        state.dispatch.textarea.elements().is_empty(),
        "chip must be inlined"
    );
    assert_eq!(state.dispatch.text(), pasted);
}

/// Enter with the caret right after a paste chip still dispatches
/// (preview shows there; expand is on-chip only).
#[test]
fn enter_after_dispatch_paste_chip_dispatches() {
    let mut state = DashboardState::new();
    let reg = crate::actions::ActionRegistry::defaults();
    let pasted = "line one\nline two\nline three\nline four";
    let _ = state.handle_input(&Event::Paste(pasted.to_string()), &reg);
    // handle_paste leaves the cursor after the chip.
    match state.handle_key(&KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &reg) {
        InputOutcome::Action(Action::DashboardDispatch { text, .. }) => {
            assert_eq!(text, pasted);
        }
        other => panic!("Enter after chip must dispatch, got {other:?}"),
    }
}

/// Peek reply: Enter on a paste chip expands rather than sending.
#[test]
fn enter_on_peek_reply_paste_chip_expands() {
    let mut state = state_with_open_peek();
    let reg = crate::actions::ActionRegistry::defaults();
    state.peek.as_mut().unwrap().focused = true;
    let pasted = "a\nb\nc";
    // Peek reply is compact → 2-line threshold; 3 lines still chips.
    let _ = state.handle_input(&Event::Paste(pasted.to_string()), &reg);
    assert_eq!(state.peek_reply.textarea.elements().len(), 1);
    state.peek_reply.set_cursor(0);
    let outcome = state.handle_key(&KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &reg);
    assert!(
        matches!(outcome, InputOutcome::Changed),
        "Enter on peek chip must expand, got {outcome:?}"
    );
    assert!(state.peek_reply.textarea.elements().is_empty());
    assert_eq!(state.peek_reply.text(), pasted);
}

/// Multiline peek still expands paste chips before treating bare Enter
/// as a newline (dispatch + agent order).
#[test]
fn multiline_peek_enter_on_paste_chip_expands() {
    let mut state = state_with_open_peek();
    state.multiline_mode = true;
    let reg = crate::actions::ActionRegistry::defaults();
    state.peek.as_mut().unwrap().focused = true;
    let pasted = "a\nb\nc";
    let _ = state.handle_input(&Event::Paste(pasted.to_string()), &reg);
    assert_eq!(state.peek_reply.textarea.elements().len(), 1);
    state.peek_reply.set_cursor(0);
    let outcome = state.handle_key(&KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &reg);
    assert!(
        matches!(outcome, InputOutcome::Changed),
        "multiline Enter on peek chip must expand, got {outcome:?}"
    );
    assert!(
        state.peek_reply.textarea.elements().is_empty(),
        "chip must be inlined, not left as an element"
    );
    assert_eq!(state.peek_reply.text(), pasted);
    assert!(
        !state.peek_reply.text().contains("\n\n"),
        "must expand, not insert an extra newline: {:?}",
        state.peek_reply.text()
    );
}

/// Enter on an image chip still dispatches — dashboard has no image
/// viewer, so ImagePreview must not swallow the key.
#[test]
fn enter_on_dispatch_image_chip_dispatches() {
    let mut state = DashboardState::new();
    let reg = crate::actions::ActionRegistry::defaults();
    let _ = state.attach_pasted_image(peek_test_image());
    state.dispatch.set_cursor(0);
    match state.handle_key(&KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &reg) {
        InputOutcome::Action(Action::DashboardDispatch { text, .. }) => {
            assert!(text.contains("[Image #1]"), "got {text:?}");
        }
        other => panic!("Enter on image chip must dispatch, got {other:?}"),
    }
}

#[test]
fn set_peek_clears_prompt_click_timer() {
    let mut state = DashboardState::new();
    state.last_prompt_click = Some(Instant::now());
    state.set_peek(Some(super::super::peek::PeekPanelState::new(
        DashboardRowId::TopLevel(AgentId(0)),
        peek_fields_for_test("Idle"),
    )));
    assert!(
        state.last_prompt_click.is_none(),
        "opening peek must clear the shared double-click timer"
    );
    state.last_prompt_click = Some(Instant::now());
    state.set_peek(None);
    assert!(
        state.last_prompt_click.is_none(),
        "closing peek must clear the shared double-click timer"
    );
}

#[test]
fn peek_row_change_clears_prompt_click_timer() {
    let mut state = state_with_open_peek();
    state.last_prompt_click = Some(Instant::now());
    state.clear_peek_reply();
    assert!(
        state.last_prompt_click.is_none(),
        "row change / clear_peek_reply must clear double-click timer"
    );
}

#[test]
fn enter_on_reject_feedback_paste_chip_expands() {
    let mut state = DashboardState::new();
    let reg = crate::actions::ActionRegistry::defaults();
    let mut f = peek_fields_for_test("Awaiting your input");
    f.question = Some("Allow Edit?".into());
    f.options = vec![
        ("allow".into(), "Allow".into()),
        ("no".into(), "No, reject".into()),
    ];
    f.reject_option = Some(1);
    f.request_id = Some(7);
    state.set_peek(Some(super::super::peek::PeekPanelState::new(
        DashboardRowId::TopLevel(AgentId(0)),
        f,
    )));
    if let Some(p) = state.peek.as_mut() {
        p.focused = true;
        p.selected_option = Some(1);
    }
    let pasted = "reason line one\nreason line two\nreason line three";
    let _ = state.handle_input(&Event::Paste(pasted.to_string()), &reg);
    assert_eq!(state.peek_reply.textarea.elements().len(), 1);
    state.peek_reply.set_cursor(0);
    let outcome = state.handle_key(&KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &reg);
    assert!(
        matches!(outcome, InputOutcome::Changed),
        "Enter on reject freeform paste chip must expand, got {outcome:?}"
    );
    assert!(state.peek_reply.textarea.elements().is_empty());
    assert_eq!(state.peek_reply.text(), pasted);
}

#[test]
fn wrap_host_image_none_paste_not_inserted_as_text() {
    let mut state = DashboardState::new();
    let reg = crate::actions::ActionRegistry::defaults();
    let outcome = state.handle_input(
        &Event::Paste(crate::wrap_clipboard_image::MAGIC_NONE.to_string()),
        &reg,
    );
    assert!(matches!(outcome, InputOutcome::Unchanged));
    assert!(state.dispatch.text().is_empty());
}

#[test]
fn wrap_host_image_none_paste_not_inserted_into_peek_reply() {
    let mut state = DashboardState::new();
    let reg = crate::actions::ActionRegistry::defaults();
    state.focus_row(DashboardRowId::TopLevel(AgentId(0)));
    state.set_peek(Some(super::super::peek::PeekPanelState::new(
        DashboardRowId::TopLevel(AgentId(0)),
        peek_fields_for_test("Idle"),
    )));
    let outcome = state.handle_input(
        &Event::Paste(crate::wrap_clipboard_image::MAGIC_NONE.to_string()),
        &reg,
    );
    assert!(matches!(outcome, InputOutcome::Unchanged));
    assert!(state.peek_reply.text().is_empty());
    assert!(state.dispatch.text().is_empty());
}

/// Wrap host-image bracketed paste with peek open must attach to
/// `peek_reply`, not the hidden new-session dispatch input.
#[test]
fn wrap_host_image_paste_with_peek_open_goes_to_reply_not_dispatch() {
    let mut state = state_with_open_peek();
    let reg = crate::actions::ActionRegistry::defaults();
    let png = test_png_bytes();
    let b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &png);
    let paste = format!(
        "{}\nimage/png\n{b64}",
        crate::wrap_clipboard_image::MAGIC_IMG
    );
    let outcome = state.handle_input(&Event::Paste(paste), &reg);
    assert!(matches!(outcome, InputOutcome::Changed));
    let text = state.peek_reply.text();
    assert!(text.contains("[Image #1]"), "got {text:?}");
    assert_eq!(state.peek_reply.images.len(), 1);
    assert!(
        state.dispatch.images.is_empty() && state.dispatch.text().is_empty(),
        "wrap image must not leak into hidden dispatch"
    );
    assert!(
        state.peek.as_ref().unwrap().focused,
        "wrap image paste must focus the reply"
    );
}

/// Question mode is text-only on the wire — wrap host-image must not
/// attach to peek reply (or leak into dispatch).
#[test]
fn wrap_host_image_paste_question_mode_blocks_attach() {
    let mut state = state_with_open_peek();
    let reg = crate::actions::ActionRegistry::defaults();
    if let Some(p) = state.peek.as_mut() {
        p.focused = true;
        p.question = Some("Allow?".into());
        p.options = vec![("yes".into(), "Yes".into()), ("no".into(), "No".into())];
        p.reject_option = Some(1);
        p.selected_option = Some(1);
    }
    let png = test_png_bytes();
    let b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &png);
    let paste = format!(
        "{}\nimage/png\n{b64}",
        crate::wrap_clipboard_image::MAGIC_IMG
    );
    let outcome = state.handle_input(&Event::Paste(paste), &reg);
    assert!(matches!(outcome, InputOutcome::Unchanged));
    assert!(state.peek_reply.images.is_empty());
    assert!(!state.peek_reply.text().contains("[Image #"));
    assert!(state.dispatch.images.is_empty() && state.dispatch.text().is_empty());
}

/// A bracketed paste while the peek panel is open lands in the
/// peek's `❯ reply` widget — NOT the hidden new-session dispatch
/// input. (Regression: terminals with bracketed paste deliver
/// Cmd/Ctrl+V as `Event::Paste`, which used to fall through to
/// the dispatch arm and silently fill the box behind the
/// panel.) A multi-line paste folds into a single `[Pasted: N
/// lines]` chip (the reply widget is compact → 2-line threshold)
/// while preserving the raw text for the eventual send, and pasting
/// focuses the input like the Ctrl/Cmd+V chord does.
#[test]
fn bracketed_paste_with_peek_open_goes_to_reply() {
    let mut state = DashboardState::new();
    let reg = crate::actions::ActionRegistry::defaults();
    state.focus_row(DashboardRowId::TopLevel(AgentId(0)));
    state.set_peek(Some(super::super::peek::PeekPanelState::new(
        DashboardRowId::TopLevel(AgentId(0)),
        peek_fields_for_test("Idle"),
    )));
    // Unfocused (Tab → row nav) — paste must still target the reply
    // and re-focus it ("pasting implies an intent to reply").
    state.peek.as_mut().unwrap().focused = false;
    let outcome = state.handle_input(&Event::Paste("hello\nworld".to_string()), &reg);
    assert!(matches!(outcome, InputOutcome::Changed));
    assert_eq!(
        state.peek_reply.text(),
        "hello\nworld",
        "paste must land in the reply with its raw text preserved",
    );
    assert_eq!(
        state.peek_reply.textarea.elements().len(),
        1,
        "a 2-line paste must fold into one [Pasted: N lines] chip (compact threshold)",
    );
    assert!(
        state.peek.as_ref().unwrap().focused,
        "paste must focus the reply input"
    );
    assert!(
        state.dispatch.text().is_empty(),
        "paste must NOT leak into the hidden dispatch input, got {:?}",
        state.dispatch.text(),
    );
}

/// A pasted image attaches as a clean `[Image #N]` chip with no
/// embedded source path (the path would blow out the single-line
/// dispatch box and the dispatched session's scrollback).
#[test]
fn pasted_image_chip_omits_full_path() {
    let mut state = DashboardState::new();
    let pasted = crate::prompt_images::PastedImage {
        element_id: echo_agent_ratatui_textarea::ElementId::from_raw(0),
        display_number: 0,
        mime_type: "image/png".into(),
        dimensions: Some((10, 10)),
        byte_len: 16,
        encoded_bytes: Some(vec![0u8; 16].into()),
        source_path: Some(std::path::PathBuf::from(
            "/Users/somebody/very/long/path/screenshot.png",
        )),
        staged_temp_path: None,
        session_image_path: None,
        preview: crate::prompt_images::PromptImagePreview::default(),
    };
    let _ = state.attach_pasted_image(pasted);
    let text = state.dispatch.text();
    assert!(
        text.contains("[Image #1]"),
        "expected a clean chip, got {text:?}"
    );
    assert!(
        !text.contains("screenshot.png"),
        "chip must not embed the source path, got {text:?}"
    );
    assert_eq!(
        state.dispatch.images[0].source_path.as_deref(),
        Some(std::path::Path::new(
            "/Users/somebody/very/long/path/screenshot.png"
        ))
    );
}

// -----------------------------------------------------------------
// The clipboard raster/file-url probe (osascript) + image decode +
// session persist run OFF the event loop. A paste that would probe
// enqueues a `ProbeClipboardAttachment` effect and returns without an
// inline probe (`clipboard_probe_call_count() == 0`); the chip
// attaches later via `complete_clipboard_attachment_paste`. Snapshot /
// support are faked via the test-only seam; plain text with no
// raster stays fully synchronous (no defer).
// -----------------------------------------------------------------

fn probe_image_data() -> crate::clipboard::ImageData {
    crate::clipboard::ImageData {
        data: test_png_bytes(),
        mime_type: "image/png".into(),
    }
}

fn ctrl_v_event() -> Event {
    Event::Key(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::CONTROL))
}

/// Target of the enqueued deferred-probe effect, if any.
fn deferred_probe_target(
    state: &DashboardState,
) -> Option<crate::app::actions::ClipboardPasteTarget> {
    deferred_probe_ctx(state).map(|ctx| ctx.target)
}

/// The `ClipboardPasteContext` of the enqueued deferred-probe effect, if any.
fn deferred_probe_ctx(
    state: &DashboardState,
) -> Option<crate::app::actions::ClipboardPasteContext> {
    state.pending_effects.iter().find_map(|e| match e {
        crate::app::actions::Effect::ProbeClipboardAttachment { ctx, .. } => Some(ctx.clone()),
        _ => None,
    })
}

/// A ready-to-insert (already decoded) pasted image for completion tests.
fn completion_pasted_image() -> crate::prompt_images::PastedImage {
    crate::prompt_images::from_clipboard_data(&probe_image_data())
}

/// A `ClipboardPasteContext` for driving `complete_clipboard_attachment_paste`
/// directly (image-wins Cmd+V: carries the caption, inserts on a no-image
/// miss). The peek target stamps the row `state_with_open_peek` peeks.
fn completion_ctx(
    clipboard_text: Option<&str>,
    peek: bool,
) -> crate::app::actions::ClipboardPasteContext {
    crate::app::actions::ClipboardPasteContext {
        target: if peek {
            crate::app::actions::ClipboardPasteTarget::DashboardPeek {
                row: DashboardRowId::TopLevel(AgentId(0)),
            }
        } else {
            crate::app::actions::ClipboardPasteTarget::DashboardDispatch
        },
        source: crate::app::actions::ClipboardPasteSource::ClipboardKey {
            text: crate::app::actions::ClipboardTextRead::Success(
                clipboard_text.map(str::to_owned),
            ),
            tip_showing: false,
        },
    }
}

/// Drive a real Cmd+V that finds a raster (defers), then complete the probe
/// with a decoded image — the full shipped deferred image-paste path. The
/// caller sets `state` up so `handle_input` routes to the intended surface
/// (peek open → reply; peek closed → dispatch).
fn cmd_v_image(state: &mut DashboardState, clipboard_text: Option<&str>) {
    let reg = crate::actions::ActionRegistry::defaults();
    crate::clipboard::set_clipboard_probe_hook(crate::clipboard::ClipboardProbeHook {
        text: clipboard_text.map(str::to_owned),
        ..crate::clipboard::ClipboardProbeHook::with_raster(None)
    });
    let _ = state.handle_input(&ctrl_v_event(), &reg);
    let ctx = deferred_probe_ctx(state).expect("an image paste must defer a probe");
    crate::clipboard::clear_clipboard_probe_hook();
    state.complete_clipboard_attachment_paste(
        ctx,
        crate::app::actions::ProbedAttachment::Image(completion_pasted_image()),
        None,
    );
}

#[test]
fn dispatch_bracketed_image_paste_defers_probe() {
    let mut state = DashboardState::new();
    let reg = crate::actions::ActionRegistry::defaults();
    // Empty bracketed paste + a raster on the pasteboard.
    crate::clipboard::set_clipboard_probe_hook(crate::clipboard::ClipboardProbeHook::with_raster(
        None,
    ));
    let _ = state.handle_input(&Event::Paste(String::new()), &reg);
    let calls = crate::clipboard::clipboard_probe_call_count();
    let target = deferred_probe_target(&state);
    crate::clipboard::clear_clipboard_probe_hook();

    assert_eq!(calls, 0, "probe must NOT run inline on the event loop");
    assert!(
        matches!(
            target,
            Some(crate::app::actions::ClipboardPasteTarget::DashboardDispatch)
        ),
        "a dispatch-targeted probe effect must be enqueued"
    );
    assert!(
        state.dispatch.images.is_empty(),
        "chip attaches on completion, not inline"
    );
}

/// Regression: an IME commit delivered as bracketed paste (Otty)
/// must not attach the unrelated clipboard image.
#[test]
fn dispatch_bracketed_paste_stamps_ctx_bracketed() {
    let mut state = DashboardState::new();
    let reg = crate::actions::ActionRegistry::defaults();
    crate::clipboard::set_clipboard_probe_hook(crate::clipboard::ClipboardProbeHook::with_raster(
        None,
    ));
    let _ = state.handle_input(&Event::Paste("中".to_owned()), &reg);
    let ctx = deferred_probe_ctx(&state);
    crate::clipboard::clear_clipboard_probe_hook();

    let ctx = ctx.expect("a bracketed paste with a raster must defer a probe");
    assert!(
        ctx.source.is_bracketed(),
        "bracketed source must let the probe verify payload origin"
    );
    assert_eq!(ctx.source.text(), Some("中"));
}

#[test]
fn dispatch_cmd_v_probe_ctx_not_bracketed() {
    let mut state = DashboardState::new();
    let reg = crate::actions::ActionRegistry::defaults();
    crate::clipboard::set_clipboard_probe_hook(crate::clipboard::ClipboardProbeHook {
        text: Some("caption".to_owned()),
        ..crate::clipboard::ClipboardProbeHook::with_raster(None)
    });
    let _ = state.handle_input(&ctrl_v_event(), &reg);
    let ctx = deferred_probe_ctx(&state);
    crate::clipboard::clear_clipboard_probe_hook();

    let ctx = ctx.expect("a Cmd+V with a raster must defer a probe");
    assert!(
        !ctx.source.is_bracketed(),
        "Cmd+V source must remain a CLIPBOARD-key read"
    );
}

/// Bracketed caption + raster: image wins across the deferral boundary — the
/// caption is NOT inserted synchronously (it is carried into the effect and
/// dropped when the probe returns an image), so the dashboard bracketed path
/// attaches exactly one thing: the image, never image + caption.
#[test]
fn dispatch_bracketed_caption_image_wins_no_double_insert() {
    let mut state = DashboardState::new();
    let reg = crate::actions::ActionRegistry::defaults();
    crate::clipboard::set_clipboard_probe_hook(crate::clipboard::ClipboardProbeHook::with_raster(
        None,
    ));
    let _ = state.handle_input(&Event::Paste("a caption".to_string()), &reg);
    let ctx = deferred_probe_ctx(&state);
    crate::clipboard::clear_clipboard_probe_hook();

    let ctx = ctx.expect("caption + raster must defer a probe");
    assert_eq!(
        state.dispatch.text(),
        "",
        "caption must not be inserted synchronously (image wins)"
    );
    assert_eq!(ctx.source.text(), Some("a caption"));
    assert!(matches!(
        ctx.source,
        crate::app::actions::ClipboardPasteSource::BracketedDeferred { .. }
    ));

    // Probe finds the image → image wins, caption dropped (no double insert).
    state.complete_clipboard_attachment_paste(
        ctx,
        crate::app::actions::ProbedAttachment::Image(completion_pasted_image()),
        None,
    );
    assert_eq!(state.dispatch.images.len(), 1);
    assert!(state.dispatch.text().contains("[Image #1]"));
    assert!(!state.dispatch.text().contains("a caption"));
}

#[test]
fn dispatch_paste_key_image_defers_probe() {
    let mut state = DashboardState::new();
    let reg = crate::actions::ActionRegistry::defaults();
    crate::clipboard::set_clipboard_probe_hook(crate::clipboard::ClipboardProbeHook::with_raster(
        None,
    ));
    let _ = state.handle_input(&ctrl_v_event(), &reg);
    let calls = crate::clipboard::clipboard_probe_call_count();
    let target = deferred_probe_target(&state);
    crate::clipboard::clear_clipboard_probe_hook();

    assert_eq!(calls, 0, "probe must NOT run inline on the event loop");
    assert!(matches!(
        target,
        Some(crate::app::actions::ClipboardPasteTarget::DashboardDispatch)
    ));
    assert!(state.dispatch.images.is_empty());
}

#[test]
fn peek_bracketed_image_paste_defers_probe() {
    let mut state = state_with_open_peek();
    let reg = crate::actions::ActionRegistry::defaults();
    crate::clipboard::set_clipboard_probe_hook(crate::clipboard::ClipboardProbeHook::with_raster(
        None,
    ));
    let _ = state.handle_input(&Event::Paste(String::new()), &reg);
    let calls = crate::clipboard::clipboard_probe_call_count();
    let target = deferred_probe_target(&state);
    crate::clipboard::clear_clipboard_probe_hook();

    assert_eq!(calls, 0, "probe must NOT run inline on the event loop");
    assert!(
        matches!(
            target,
            Some(crate::app::actions::ClipboardPasteTarget::DashboardPeek { .. })
        ),
        "a peek-targeted probe effect must be enqueued"
    );
    assert!(state.peek_reply.images.is_empty());
}

#[test]
fn peek_paste_key_image_defers_probe() {
    let mut state = state_with_open_peek();
    let reg = crate::actions::ActionRegistry::defaults();
    crate::clipboard::set_clipboard_probe_hook(crate::clipboard::ClipboardProbeHook::with_raster(
        None,
    ));
    let _ = state.handle_input(&ctrl_v_event(), &reg);
    let calls = crate::clipboard::clipboard_probe_call_count();
    let target = deferred_probe_target(&state);
    crate::clipboard::clear_clipboard_probe_hook();

    assert_eq!(calls, 0, "probe must NOT run inline on the event loop");
    assert!(matches!(
        target,
        Some(crate::app::actions::ClipboardPasteTarget::DashboardPeek { .. })
    ));
    assert!(state.peek_reply.images.is_empty());
}

#[test]
fn dispatch_text_paste_no_raster_stays_synchronous() {
    let mut state = DashboardState::new();
    let reg = crate::actions::ActionRegistry::defaults();
    // Non-empty non-URL text with no raster: the snapshot gate skips the
    // probe entirely, so nothing is deferred and the text inserts inline.
    crate::clipboard::set_clipboard_probe_hook(crate::clipboard::ClipboardProbeHook::no_raster(
        None,
    ));
    let outcome = state.handle_input(&Event::Paste("hello world".to_string()), &reg);
    let calls = crate::clipboard::clipboard_probe_call_count();
    let deferred = deferred_probe_target(&state).is_some();
    crate::clipboard::clear_clipboard_probe_hook();

    assert!(matches!(outcome, InputOutcome::Changed));
    assert_eq!(calls, 0, "no inline probe");
    assert!(
        !deferred,
        "plain text with no raster must not defer a probe"
    );
    assert_eq!(
        state.dispatch.text(),
        "hello world",
        "text inserted synchronously"
    );
}

#[test]
fn dispatch_path_paste_attaches_inline_without_deferring() {
    let dir = tempfile::tempdir().unwrap();
    let png = write_test_png(dir.path());
    let mut state = DashboardState::new();
    let reg = crate::actions::ActionRegistry::defaults();
    // Even with a raster snapshot, a pasted file path resolves inline and
    // must NOT also enqueue a probe (no double insert).
    crate::clipboard::set_clipboard_probe_hook(crate::clipboard::ClipboardProbeHook::with_raster(
        None,
    ));
    let outcome = state.handle_input(&Event::Paste(png.display().to_string()), &reg);
    let calls = crate::clipboard::clipboard_probe_call_count();
    let deferred = deferred_probe_target(&state).is_some();
    crate::clipboard::clear_clipboard_probe_hook();

    assert!(matches!(outcome, InputOutcome::Changed));
    assert_eq!(calls, 0, "no inline probe");
    assert!(
        !deferred,
        "a resolved path paste must not also defer a probe"
    );
    assert_eq!(
        state.dispatch.images.len(),
        1,
        "path attached as a chip inline"
    );
    assert!(state.dispatch.text().contains("[Image #1]"));
}

#[test]
fn completion_attaches_image_to_dispatch() {
    let mut state = DashboardState::new();
    let completion = state.complete_clipboard_attachment_paste(
        completion_ctx(Some("caption"), false),
        crate::app::actions::ProbedAttachment::Image(completion_pasted_image()),
        None,
    );
    assert_eq!(
        completion,
        crate::app::actions::ClipboardPasteCompletion::Handled
    );
    assert_eq!(state.dispatch.images.len(), 1);
    assert!(state.dispatch.text().contains("[Image #1]"));
    // Image wins: the carried caption is NOT also inserted (no double).
    assert!(!state.dispatch.text().contains("caption"));
}

#[test]
fn completion_attaches_image_to_peek() {
    let mut state = state_with_open_peek();
    state.complete_clipboard_attachment_paste(
        completion_ctx(None, true),
        crate::app::actions::ProbedAttachment::Image(completion_pasted_image()),
        None,
    );
    assert_eq!(state.peek_reply.images.len(), 1);
    assert!(state.peek_reply.text().contains("[Image #1]"));
    assert!(
        state.dispatch.images.is_empty(),
        "peek completion must not leak into dispatch"
    );
}

/// A no-image miss on an image-wins dispatch Cmd+V inserts the carried
/// caption instead — deferring the probe must not lose a text-only paste.
#[test]
fn completion_inserts_caption_on_no_image_miss() {
    let mut state = DashboardState::new();
    let completion = state.complete_clipboard_attachment_paste(
        completion_ctx(Some("just some text"), false),
        crate::app::actions::ProbedAttachment::NoRaster,
        None,
    );
    assert_eq!(
        completion,
        crate::app::actions::ClipboardPasteCompletion::Handled
    );
    assert!(state.dispatch.images.is_empty());
    assert_eq!(state.dispatch.text(), "just some text");
}

#[test]
fn dashboard_deferred_bracketed_text_survives_failed_or_dropped_probe() {
    use crate::app::actions::{ClipboardPasteCompletion, ClipboardPasteFailure, ProbedAttachment};
    for (probe, expected) in [
        (
            ProbedAttachment::ProbeFailed,
            ClipboardPasteCompletion::Failed(ClipboardPasteFailure::AttachmentRead),
        ),
        (
            ProbedAttachment::ProbeDropped,
            ClipboardPasteCompletion::Dropped,
        ),
    ] {
        let mut state = DashboardState::new();
        let mut ctx = completion_ctx(None, false);
        ctx.source = crate::app::actions::ClipboardPasteSource::BracketedDeferred {
            text: "bracketed text".to_owned(),
        };

        let completion = state.complete_clipboard_attachment_paste(ctx, probe, None);

        assert_eq!(completion, expected);
        assert_eq!(state.dispatch.text(), "bracketed text");
    }
}

/// A no-image miss on a peek Cmd+V must NOT buffer the caption into the
/// hidden reply if the peeked agent raised a question during the probe window
/// (the reply is text-only on the wire in question mode).
#[test]
fn completion_peek_caption_dropped_in_question_mode() {
    let mut state = state_with_open_peek();
    state.paste_probe_in_flight = 1;
    if let Some(p) = state.peek.as_mut() {
        p.question = Some("Allow?".into());
    }
    let completion = state.complete_clipboard_attachment_paste(
        completion_ctx(Some("some caption"), true),
        crate::app::actions::ProbedAttachment::NoRaster,
        None,
    );
    assert_eq!(
        completion,
        crate::app::actions::ClipboardPasteCompletion::Dropped
    );
    assert!(
        state.peek_reply.text().is_empty(),
        "the caption must be dropped in question mode, not buffered into the hidden reply"
    );
    assert_eq!(state.paste_probe_in_flight, 0);
}

#[test]
fn completion_attaches_file_url_to_dispatch() {
    let dir = tempfile::tempdir().unwrap();
    let png = write_test_png(dir.path());
    let mut state = DashboardState::new();
    let completion = state.complete_clipboard_attachment_paste(
        completion_ctx(None, false),
        crate::app::actions::ProbedAttachment::NoRaster,
        Some(png.display().to_string()),
    );
    assert_eq!(
        completion,
        crate::app::actions::ClipboardPasteCompletion::Handled
    );
    assert_eq!(
        state.dispatch.images.len(),
        1,
        "file URL attached as a chip"
    );
    assert!(state.dispatch.text().contains("[Image #1]"));
}

/// A peek completion arriving after the panel closed drops the attachment
/// instead of inserting into the now-hidden reply buffer.
#[test]
fn completion_peek_dropped_when_panel_closed() {
    let mut state = DashboardState::new(); // no peek open
    state.paste_probe_in_flight = 1;
    let completion = state.complete_clipboard_attachment_paste(
        completion_ctx(None, true),
        crate::app::actions::ProbedAttachment::Image(completion_pasted_image()),
        None,
    );
    assert_eq!(
        completion,
        crate::app::actions::ClipboardPasteCompletion::Dropped
    );
    assert!(
        state.peek_reply.images.is_empty(),
        "a closed peek must not receive the deferred image"
    );
    assert_eq!(
        state.paste_probe_in_flight, 0,
        "the in-flight count is still decremented so a stashed send can drain"
    );
}

/// A peek completion arriving after the panel moved to ANOTHER row drops
/// the attachment (never lands in a different agent's reply), and a peek
/// send stashed for the old row is dropped at drain time — with a toast —
/// instead of replying to the newly peeked agent.
#[test]
fn completion_peek_dropped_when_row_changed() {
    let mut state = state_with_open_peek(); // peeks TopLevel(AgentId(0))
    state.paste_probe_in_flight = 1;
    state.deferred_peek_send = Some(DeferredPeekSend {
        row: DashboardRowId::TopLevel(AgentId(0)),
        attach: false,
    });
    // The user moves the peek to a different row during the probe window.
    if let Some(p) = state.peek.as_mut() {
        p.row = DashboardRowId::TopLevel(AgentId(7));
    }
    state.complete_clipboard_attachment_paste(
        completion_ctx(None, true),
        crate::app::actions::ProbedAttachment::Image(completion_pasted_image()),
        None,
    );
    assert!(
        state.peek_reply.images.is_empty(),
        "a retargeted peek must not receive the deferred image"
    );
    assert_eq!(state.paste_probe_in_flight, 0);
    let actions = state.take_deferred_sends_after_paste();
    assert!(
        actions.is_empty(),
        "the stale peek stash must not reissue to the new row: {actions:?}"
    );
    assert!(state.deferred_peek_send.is_none(), "the stash is consumed");
    assert!(
        state.error_toast.is_some(),
        "dropping the stashed reply must be announced with a toast"
    );
}

/// A question arriving on the peeked row mid-probe makes the reply
/// text-only: an Image completion for that row must be discarded WITH a
/// toast (not silently no-opped by the attach helper's question guard).
#[test]
fn completion_peek_image_discarded_when_question_arrives() {
    let mut state = state_with_open_peek();
    let reg = crate::actions::ActionRegistry::defaults();
    // Cmd+V in NORMAL mode → the probe defers against the peeked row.
    crate::clipboard::set_clipboard_probe_hook(crate::clipboard::ClipboardProbeHook::with_raster(
        None,
    ));
    let _ = state.handle_input(&ctrl_v_event(), &reg);
    let ctx = deferred_probe_ctx(&state).expect("an image paste must defer a probe");
    crate::clipboard::clear_clipboard_probe_hook();
    assert_eq!(state.paste_probe_in_flight, 1);
    // A permission question arrives on the SAME row before completion.
    if let Some(p) = state.peek.as_mut() {
        p.question = Some("Allow?".into());
    }
    let completion = state.complete_clipboard_attachment_paste(
        ctx,
        crate::app::actions::ProbedAttachment::Image(completion_pasted_image()),
        None,
    );
    assert_eq!(
        completion,
        crate::app::actions::ClipboardPasteCompletion::Dropped
    );
    assert!(
        state.peek_reply.images.is_empty(),
        "the image must not attach to a question-mode reply"
    );
    assert!(!state.peek_reply.text().contains("[Image #"));
    assert!(
        state.error_toast.is_some(),
        "discarding the deferred image must be announced with a toast"
    );
    assert_eq!(state.paste_probe_in_flight, 0);
}

#[test]
fn completion_reports_full_miss_for_unreadable_file_url() {
    let mut state = DashboardState::new();
    let completion = state.complete_clipboard_attachment_paste(
        completion_ctx(None, false),
        crate::app::actions::ProbedAttachment::NoRaster,
        Some("file:///definitely/missing/echo-agent-primary-paste.png".to_owned()),
    );

    assert_eq!(
        completion,
        crate::app::actions::ClipboardPasteCompletion::FullMiss
    );
    assert!(state.dispatch.text().is_empty());
    assert!(state.dispatch.images.is_empty());
}

#[test]
fn completion_reports_failed_probe() {
    let mut state = DashboardState::new();
    let completion = state.complete_clipboard_attachment_paste(
        completion_ctx(None, false),
        crate::app::actions::ProbedAttachment::ProbeFailed,
        None,
    );

    assert_eq!(
        completion,
        crate::app::actions::ClipboardPasteCompletion::Failed(
            crate::app::actions::ClipboardPasteFailure::AttachmentRead,
        )
    );
}

/// Same guard for the file-url completion arm: Finder file-URL chips are
/// attachments too and must be discarded loudly in question mode.
#[test]
fn completion_peek_file_urls_discarded_when_question_arrives() {
    let dir = tempfile::tempdir().unwrap();
    let png = write_test_png(dir.path());
    let mut state = state_with_open_peek();
    state.paste_probe_in_flight = 1;
    if let Some(p) = state.peek.as_mut() {
        p.question = Some("Allow?".into());
    }
    state.complete_clipboard_attachment_paste(
        completion_ctx(None, true),
        crate::app::actions::ProbedAttachment::NoRaster,
        Some(png.display().to_string()),
    );
    assert!(
        state.peek_reply.images.is_empty(),
        "file-url chips must not attach to a question-mode reply"
    );
    assert!(
        state.error_toast.is_some(),
        "discarding the deferred file-url chips must be announced with a toast"
    );
    assert_eq!(state.paste_probe_in_flight, 0);
}

/// A peek send stashed in normal mode must NOT reissue once a question
/// owns the panel — the reply dispatch would silently queue a prompt and
/// wipe the draft behind the dialog. It is dropped with a toast and the
/// draft stays in the widget.
#[test]
fn stashed_peek_reply_dropped_when_question_active() {
    let mut state = state_with_open_peek(); // peeks TopLevel(AgentId(0))
    state.peek_reply.set_text("please look");
    state.deferred_peek_send = Some(DeferredPeekSend {
        row: DashboardRowId::TopLevel(AgentId(0)),
        attach: false,
    });
    if let Some(p) = state.peek.as_mut() {
        p.question = Some("Allow?".into());
    }
    let actions = state.take_deferred_sends_after_paste();
    assert!(
        actions.is_empty(),
        "the stashed reply must not reissue into question mode: {actions:?}"
    );
    assert!(state.deferred_peek_send.is_none(), "the stash is consumed");
    assert!(
        state.error_toast.is_some(),
        "dropping the stashed reply must be announced with a toast"
    );
    assert_eq!(
        state.peek_reply.text(),
        "please look",
        "the draft stays in the widget for after the question"
    );
}

// -----------------------------------------------------------------
// `/` literal + `Ctrl+/` search mode (replaces the old `/`→filter
// behaviour that silently swallowed prompts starting with a
// filter prefix).
// -----------------------------------------------------------------

/// `/` types a literal slash into the prompt — it no longer
/// enters a filter mode. (Filtering moved to `Ctrl+/`.)
#[test]
fn slash_types_literal_not_filter() {
    let mut state = DashboardState::new();
    let reg = crate::actions::ActionRegistry::defaults();
    let ev = Event::Key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE));
    let _ = state.handle_input(&ev, &reg);
    assert_eq!(state.dispatch.text(), "/", "/ must type a literal slash");
    assert!(!state.search_mode, "/ must NOT enter search mode");
    assert!(
        matches!(state.filter, Filter::None),
        "/ must NOT set a filter",
    );
}

/// `Ctrl+/` toggles search mode on, then off.
#[test]
fn ctrl_slash_toggles_search_mode() {
    let mut state = DashboardState::new();
    let reg = crate::actions::ActionRegistry::defaults();
    let ctrl_slash = Event::Key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::CONTROL));
    let o1 = state.handle_input(&ctrl_slash, &reg);
    assert!(matches!(o1, InputOutcome::Changed));
    assert!(state.search_mode, "Ctrl+/ must enter search mode");
    let o2 = state.handle_input(&ctrl_slash, &reg);
    assert!(matches!(o2, InputOutcome::Changed));
    assert!(!state.search_mode, "Ctrl+/ again must exit search mode");
}

/// In search mode the dispatch buffer is a live filter query;
/// Enter CONFIRMS (keeps the filter, leaves search, clears query).
#[test]
fn search_mode_typing_filters_live_and_enter_confirms() {
    let mut state = DashboardState::new();
    let reg = crate::actions::ActionRegistry::defaults();
    state.enter_search_mode();
    for ch in "auth".chars() {
        let ev = Event::Key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        let _ = state.handle_input(&ev, &reg);
    }
    assert_eq!(state.dispatch.text(), "auth");
    assert!(
        matches!(&state.filter, Filter::Substring(s) if s == "auth"),
        "typing in search mode must update the filter live, got {:?}",
        state.filter,
    );
    let enter = Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    let outcome = state.handle_input(&enter, &reg);
    assert!(matches!(outcome, InputOutcome::Changed));
    assert!(!state.search_mode, "Enter must leave search mode");
    assert!(
        matches!(&state.filter, Filter::Substring(s) if s == "auth"),
        "Enter must KEEP the filter applied",
    );
    assert!(
        state.dispatch.text().is_empty(),
        "query buffer cleared after confirm",
    );
}

#[test]
fn search_mode_cursor_only_edit_redraws_without_filter_change() {
    let mut state = DashboardState::new();
    let registry = crate::actions::ActionRegistry::defaults();
    state.enter_search_mode();
    state.dispatch.set_text("auth");
    state.dispatch.set_cursor(0);
    state.filter = Filter::Substring("auth".to_owned());

    let outcome = state.handle_input(
        &Event::Key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE)),
        &registry,
    );
    assert!(matches!(outcome, InputOutcome::Changed));
    assert_eq!(state.dispatch.text(), "auth");
    assert_eq!(state.dispatch.cursor(), 1);
    assert!(matches!(&state.filter, Filter::Substring(text) if text == "auth"));
}

/// Esc in search mode CANCELS: clears the filter and exits.
#[test]
fn search_mode_esc_cancels_and_clears_filter() {
    let mut state = DashboardState::new();
    let reg = crate::actions::ActionRegistry::defaults();
    state.enter_search_mode();
    state.dispatch.set_text("auth");
    state.filter = Filter::Substring("auth".into());
    let esc = Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    let outcome = state.handle_input(&esc, &reg);
    assert!(matches!(outcome, InputOutcome::Changed));
    assert!(!state.search_mode, "Esc must exit search mode");
    assert!(
        matches!(state.filter, Filter::None),
        "Esc must clear the filter",
    );
    assert!(state.dispatch.text().is_empty());
}

/// In search mode, bare letters that are normally nav shortcuts
/// (j/k) type into the query instead of navigating.
#[test]
fn search_mode_bare_letter_types_into_query() {
    let mut state = DashboardState::new();
    let reg = crate::actions::ActionRegistry::defaults();
    state.enter_search_mode();
    for ch in "jk".chars() {
        let ev = Event::Key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        let _ = state.handle_input(&ev, &reg);
    }
    assert_eq!(
        state.dispatch.text(),
        "jk",
        "j/k must type in search mode, not navigate",
    );
}

/// edge case 21: Enter on empty input attaches.
#[test]
fn enter_with_empty_input_attaches() {
    let state = make_state_with_selection();
    let reg = crate::actions::ActionRegistry::defaults();
    let key = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
    let outcome = state.clone_for_test().handle_key(&key, &reg);
    assert!(matches!(
        outcome,
        InputOutcome::Action(Action::DashboardAttach(_))
    ));
}

/// Single left-click on a row attaches the
/// conversation immediately. The previous double-click-required
/// design felt unresponsive (user explicitly reported "click
/// does not respond or do anything properly"). Mouse handling
/// now mirrors click-to-open list semantics from gh-dash / k9s.
#[test]
fn single_click_on_row_attaches_immediately() {
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    use ratatui::layout::Rect;
    let mut state = make_state_with_selection();
    // Populate row_rects so the click lookup finds a row.
    let row_id = state
        .selected
        .as_ref()
        .cloned()
        .expect("seed state has a selection");
    let rect = Rect {
        x: 0,
        y: 5,
        width: 80,
        height: 1,
    };
    state.row_rects = vec![(row_id.clone(), rect)];
    let click = MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 10,
        row: 5,
        modifiers: crossterm::event::KeyModifiers::NONE,
    };
    let outcome = state.handle_mouse(&click);
    match outcome {
        InputOutcome::Action(Action::DashboardAttach(id)) => {
            assert_eq!(id, row_id, "click must attach the clicked row, got {id:?}");
        }
        other => panic!("expected DashboardAttach on single click, got {other:?}"),
    }
    // The clicked row also becomes selected so a follow-up Esc
    // returns the cursor to where the user clicked.
    assert_eq!(state.selected.as_ref(), Some(&row_id));
}

/// Clicking on an empty cell (no row at that
/// position) is a no-op. Prevents accidental attaches when the
/// user clicks between rows or in the header area.
#[test]
fn click_on_empty_area_is_no_op() {
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    let mut state = make_state_with_selection();
    // Empty row_rects → no row to find at the click position.
    state.row_rects = Vec::new();
    let click = MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 10,
        row: 5,
        modifiers: crossterm::event::KeyModifiers::NONE,
    };
    let outcome = state.handle_mouse(&click);
    assert!(
        matches!(outcome, InputOutcome::Unchanged),
        "click on empty area must be a no-op, got {outcome:?}",
    );
}

/// Clicking a model in the dashboard `/model` slash dropdown must
/// accept the completion and must NOT attach the session row that
/// sits under the same screen coordinates (click-through bug).
#[test]
fn slash_model_dropdown_click_selects_model_not_session_row() {
    use agent_client_protocol as acp;
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    use indexmap::IndexMap;
    use ratatui::layout::Rect;

    let mut state = make_state_with_selection();
    let row_id = state
        .selected
        .as_ref()
        .cloned()
        .expect("seed state has a selection");

    // Session row occupies the same Y as the model dropdown item.
    state.row_rects = vec![(
        row_id.clone(),
        Rect {
            x: 0,
            y: 5,
            width: 80,
            height: 1,
        },
    )];
    // Slash dropdown overlays that row (as `render_slash_dropdown` does).
    state.slash_dropdown_items_area = Some(Rect {
        x: 2,
        y: 5,
        width: 40,
        height: 4,
    });
    state.slash_dropdown_hit = crate::views::slash_dropdown::RenderedDropdown {
        row_items: vec![0, 1, 2, 3],
        has_scrollbar: false,
    };

    // Seed a model catalog and open `/model ` so arg suggestions exist.
    let model_id = acp::ModelId::new("beta-model");
    let mut available = IndexMap::new();
    available.insert(
        model_id.clone(),
        acp::ModelInfo::new(model_id.clone(), "Beta Model"),
    );
    available.insert(
        acp::ModelId::new("alpha-model"),
        acp::ModelInfo::new(acp::ModelId::new("alpha-model"), "Alpha Model"),
    );
    state.models.update_catalog(available);
    state.models.set_current(model_id, None);
    // Mirror how the real dashboard types into the dispatch box:
    // caret at end so `/model ` is in the args phase.
    state.dispatch.set_text("/model ");
    let end = state.dispatch.text().len();
    state.dispatch.textarea.set_cursor(end);
    state.dispatch.refresh_slash(&state.models);
    let snap = state.dispatch.slash_snapshot();
    assert!(
        !snap.matches.is_empty(),
        "expected model arg suggestions for /model "
    );

    let click = MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 10,
        row: 5, // inside BOTH dropdown and row_rects
        modifiers: crossterm::event::KeyModifiers::NONE,
    };
    let outcome = state.handle_mouse(&click);
    assert!(
        !matches!(outcome, InputOutcome::Action(Action::DashboardAttach(_))),
        "model-list click must not attach session, got {outcome:?}"
    );
    assert!(
        matches!(outcome, InputOutcome::Changed),
        "dropdown click should be consumed as Changed, got {outcome:?}"
    );
    let text = state.dispatch.text();
    assert!(
        text.contains("alpha-model")
            || text.contains("beta-model")
            || text.contains("Alpha")
            || text.contains("Beta"),
        "dispatch should contain accepted model completion, got {text:?}"
    );
    assert!(
        !state.list_focused,
        "clicking the dropdown focuses the input"
    );
}

/// Hover over the open slash dropdown updates `slash_hovered` so the
/// list tracks the pointer (agent-view parity).
#[test]
fn slash_dropdown_mouse_move_sets_hover() {
    use agent_client_protocol as acp;
    use crossterm::event::{MouseEvent, MouseEventKind};
    use indexmap::IndexMap;
    use ratatui::layout::Rect;

    let mut state = DashboardState::new();
    state.slash_dropdown_items_area = Some(Rect {
        x: 2,
        y: 5,
        width: 40,
        height: 4,
    });
    state.slash_dropdown_hit = crate::views::slash_dropdown::RenderedDropdown {
        row_items: vec![0, 1, 2, 3],
        has_scrollbar: false,
    };
    let model_id = acp::ModelId::new("hover-model");
    let mut available = IndexMap::new();
    available.insert(
        model_id.clone(),
        acp::ModelInfo::new(model_id.clone(), "Hover Model"),
    );
    state.models.update_catalog(available);
    state.models.set_current(model_id, None);
    state.dispatch.set_text("/model ");
    let end = state.dispatch.text().len();
    state.dispatch.textarea.set_cursor(end);
    state.dispatch.refresh_slash(&state.models);
    assert!(
        !state.dispatch.slash_snapshot().matches.is_empty(),
        "expected model suggestions so hover can land on a row"
    );

    let move_ev = MouseEvent {
        kind: MouseEventKind::Moved,
        column: 10,
        row: 5,
        modifiers: crossterm::event::KeyModifiers::NONE,
    };
    let _ = state.handle_mouse(&move_ev);
    assert_eq!(
        state.dispatch.slash_hovered(),
        Some(0),
        "pointer over first dropdown row should set hover index 0"
    );
}

/// With a section header selected and the prompt empty, Right
/// expands, Left collapses, and Enter toggles it.
#[test]
fn section_keys_collapse_expand_and_toggle() {
    let mut state = DashboardState::new();
    let reg = crate::actions::ActionRegistry::defaults();
    let key_sec = SectionKey::State(RowState::Working);
    state.focus_section(key_sec);
    assert_eq!(state.selected_section, Some(key_sec));

    let _ = state.handle_key(&KeyEvent::new(KeyCode::Left, KeyModifiers::NONE), &reg);
    assert!(state.is_section_collapsed(key_sec), "Left must collapse");

    let _ = state.handle_key(&KeyEvent::new(KeyCode::Right, KeyModifiers::NONE), &reg);
    assert!(!state.is_section_collapsed(key_sec), "Right must expand");

    let _ = state.handle_key(&KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &reg);
    assert!(
        state.is_section_collapsed(key_sec),
        "Enter toggles → collapsed"
    );
    let _ = state.handle_key(&KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &reg);
    assert!(
        !state.is_section_collapsed(key_sec),
        "Enter toggles → expanded"
    );
}

/// A freshly-constructed dashboard starts with the "Inactive"
/// (roster-only) section collapsed by default — and no other
/// section. Expanding it is one keypress away (and survives reopen
/// within the process; see `collapsed_sections` docs).
#[test]
fn inactive_section_collapsed_by_default() {
    let state = DashboardState::new();
    assert!(
        state.is_section_collapsed(SectionKey::State(RowState::Inactive)),
        "Inactive must start collapsed",
    );
    for other in [
        RowState::NeedsInput,
        RowState::Working,
        RowState::Idle,
        RowState::Completed,
        RowState::Failed,
        RowState::Blocked,
    ] {
        assert!(
            !state.is_section_collapsed(SectionKey::State(other)),
            "{other:?} must start expanded",
        );
    }
    assert!(
        !state.is_section_collapsed(SectionKey::Pinned),
        "Pinned must start expanded",
    );
}

/// Enter on the Idle overflow toggle flips `idle_show_all`; Right
/// reveals, Left re-folds (with an empty prompt).
#[test]
fn idle_overflow_enter_and_arrows_toggle_show_all() {
    let mut state = DashboardState::new();
    let reg = crate::actions::ActionRegistry::defaults();
    state.focus_idle_overflow();
    assert!(state.selected_idle_overflow);
    assert!(!state.idle_show_all, "starts capped");

    let _ = state.handle_key(&KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &reg);
    assert!(state.idle_show_all, "Enter reveals");
    let _ = state.handle_key(&KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &reg);
    assert!(!state.idle_show_all, "Enter re-folds");

    let _ = state.handle_key(&KeyEvent::new(KeyCode::Right, KeyModifiers::NONE), &reg);
    assert!(state.idle_show_all, "Right reveals");
    let _ = state.handle_key(&KeyEvent::new(KeyCode::Left, KeyModifiers::NONE), &reg);
    assert!(!state.idle_show_all, "Left re-folds");
}

/// vim mode ON with the LIST focused — `l` / `h` on the Idle overflow
/// toggle reveal / re-fold the folded agents (mirroring the section
/// keys). vim mode ON with the INPUT focused — they type into the
/// dispatch prompt instead of toggling.
#[test]
fn idle_overflow_vim_hl_focus_gated() {
    let reg = crate::actions::ActionRegistry::defaults();

    // vim ON + LIST focused — `l`/`h` toggle show-all.
    crate::appearance::cache::set_vim_mode(true);
    let mut state = DashboardState::new();
    state.focus_idle_overflow();
    state.list_focused = true;
    let _ = state.handle_key(&KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE), &reg);
    let show_all_after_l = state.idle_show_all;
    let _ = state.handle_key(&KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE), &reg);
    let show_all_after_h = state.idle_show_all;
    crate::appearance::cache::set_vim_mode(false);
    assert!(show_all_after_l, "list-focused vim `l` must reveal");
    assert!(!show_all_after_h, "list-focused vim `h` must re-fold");

    // vim ON + INPUT focused (list_focused == false) + empty draft —
    // `h`/`l` must type into the prompt, never toggle show-all.
    crate::appearance::cache::set_vim_mode(true);
    let mut state = DashboardState::new();
    state.focus_idle_overflow();
    // Input focused is the default; leave list_focused == false.
    let _ = state.handle_key(&KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE), &reg);
    let show_all_after_input_h = state.idle_show_all;
    let _ = state.handle_key(&KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE), &reg);
    let show_all_after_input_l = state.idle_show_all;
    let typed = state.dispatch.text().to_string();
    crate::appearance::cache::set_vim_mode(false);
    assert!(
        !show_all_after_input_h && !show_all_after_input_l,
        "input-focused vim `h`/`l` must not toggle show-all",
    );
    assert_eq!(
        typed, "hl",
        "input-focused vim `h`/`l` must type into the dispatch input",
    );
}

/// With the list focused and the Idle overflow toggle selected, Esc
/// focuses the `[+ New Agent]` button (mirroring the section / row
/// deselect tiers), rather than exiting.
#[test]
fn idle_overflow_esc_focuses_new_agent_button() {
    let mut state = DashboardState::new();
    let reg = crate::actions::ActionRegistry::defaults();
    state.focus_idle_overflow();
    state.list_focused = true;
    let _ = state.handle_key(&KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &reg);
    assert!(state.new_agent_button_focused, "Esc focuses the button");
    assert!(
        !state.selected_idle_overflow,
        "Esc clears the overflow cursor"
    );
}

/// The overflow cursor is mutually exclusive with the other three
/// cursor targets — focusing any of them clears it.
#[test]
fn focusing_other_targets_clears_idle_overflow() {
    let mut state = DashboardState::new();
    state.focus_idle_overflow();
    state.focus_new_agent_button();
    assert!(
        !state.selected_idle_overflow,
        "button focus clears overflow"
    );

    state.focus_idle_overflow();
    state.focus_section(SectionKey::State(RowState::Working));
    assert!(
        !state.selected_idle_overflow,
        "section focus clears overflow"
    );

    state.focus_idle_overflow();
    state.focus_row(DashboardRowId::TopLevel(crate::app::agent::AgentId(0)));
    assert!(!state.selected_idle_overflow, "row focus clears overflow");
}

/// `reanchor_selection` drops a stale overflow cursor (the toggle row
/// vanished because the Idle group is no longer capped) onto the
/// `[+ New Agent]` button.
#[test]
fn reanchor_clears_stale_idle_overflow_cursor() {
    let mut state = DashboardState::new();
    state.focus_idle_overflow();
    // No rows at all → no overflow focusable exists.
    state.reanchor_selection(&[]);
    assert!(
        state.new_agent_button_focused,
        "a stranded overflow cursor must fall back to the button",
    );
    assert!(!state.selected_idle_overflow);
}

/// With the list focused and a section header selected, Esc focuses
/// the `[+ New Agent]` button (mirroring the row-deselect tier),
/// rather than exiting.
#[test]
fn section_esc_focuses_new_agent_button() {
    let mut state = DashboardState::new();
    let reg = crate::actions::ActionRegistry::defaults();
    state.focus_section(SectionKey::State(RowState::Working));
    state.list_focused = true;
    let _ = state.handle_key(&KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &reg);
    assert!(
        state.new_agent_button_focused,
        "Esc on a section must focus [+ New Agent]",
    );
    assert!(
        state.selected_section.is_none(),
        "Esc must clear the section cursor",
    );
}

/// Clicking a section header selects it and toggles its collapse.
#[test]
fn section_click_selects_and_toggles() {
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    let mut state = DashboardState::new();
    let key_sec = SectionKey::State(RowState::Idle);
    // Simulate a rendered header hit rect (render rebuilds these).
    state.section_rects.push((key_sec, Rect::new(0, 0, 40, 1)));
    let click = MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 5,
        row: 0,
        modifiers: KeyModifiers::NONE,
    };
    let _ = state.handle_mouse(&click);
    assert_eq!(
        state.selected_section,
        Some(key_sec),
        "click selects the section",
    );
    assert!(state.is_section_collapsed(key_sec), "click collapses");
    // A second click expands it again (rect persists between renders).
    let _ = state.handle_mouse(&click);
    assert!(!state.is_section_collapsed(key_sec), "second click expands");
}

/// Moving the mouse over a section header sets `hovered_section`;
/// moving off clears it.
#[test]
fn section_hover_sets_and_clears_hovered_section() {
    use crossterm::event::{MouseEvent, MouseEventKind};
    let mut state = DashboardState::new();
    let key_sec = SectionKey::State(RowState::Idle);
    state.section_rects.push((key_sec, Rect::new(0, 0, 40, 1)));
    let over = MouseEvent {
        kind: MouseEventKind::Moved,
        column: 5,
        row: 0,
        modifiers: KeyModifiers::NONE,
    };
    let _ = state.handle_mouse(&over);
    assert_eq!(state.hovered_section, Some(key_sec));
    let off = MouseEvent {
        kind: MouseEventKind::Moved,
        column: 5,
        row: 5,
        modifiers: KeyModifiers::NONE,
    };
    let _ = state.handle_mouse(&off);
    assert_eq!(state.hovered_section, None);
}

/// `focus_section` / `focus_row` / `focus_new_agent_button` keep the
/// three cursor targets mutually exclusive.
#[test]
fn cursor_targets_are_mutually_exclusive() {
    let mut state = DashboardState::new();
    state.focus_section(SectionKey::Pinned);
    assert_eq!(state.selected_section, Some(SectionKey::Pinned));
    assert!(state.selected.is_none());
    assert!(!state.new_agent_button_focused);

    state.focus_row(DashboardRowId::TopLevel(crate::app::agent::AgentId(0)));
    assert!(
        state.selected_section.is_none(),
        "focus_row clears the section"
    );

    state.focus_section(SectionKey::Pinned);
    state.focus_new_agent_button();
    assert!(
        state.selected_section.is_none(),
        "focus_new_agent_button clears the section",
    );
}

/// The cheatsheet's `[✗]` chrome close button is clickable and
/// hover-tracked — mouse events must route through
/// `modal_window::handle_modal_mouse` before the picker content
/// (whose own close rect is a dead `Rect::default()`).
#[test]
fn shortcuts_modal_close_button_clicks_and_hovers() {
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    let mut state = DashboardState::new();
    let entries = Vec::new();
    let picker = crate::views::shortcuts_help::build_initial_picker_state(&entries);
    let mut modal = Box::new(ShortcutsModalState {
        entries,
        state: picker,
        window: Default::default(),
        filter_active: false,
        collapsed_sections: Default::default(),
        expanded_ids: std::collections::HashSet::new(),
        mode: crate::views::shortcuts_help::ShortcutsHelpMode::Browse,
    });
    // Simulate the rect render_close_button records each frame.
    modal.window.close_button_rect = Some(Rect::new(70, 2, 5, 1));
    state.shortcuts_modal = Some(modal);
    let reg = crate::actions::ActionRegistry::defaults();

    // Hover over the button → tracked + repaint requested.
    let over = Event::Mouse(MouseEvent {
        kind: MouseEventKind::Moved,
        column: 72,
        row: 2,
        modifiers: KeyModifiers::NONE,
    });
    assert!(matches!(
        state.handle_input(&over, &reg),
        InputOutcome::Changed
    ));
    assert!(
        state
            .shortcuts_modal
            .as_ref()
            .is_some_and(|m| m.window.close_hovered),
        "hovering [✗] must set close_hovered",
    );

    // Click on the button → close action.
    let click = Event::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 72,
        row: 2,
        modifiers: KeyModifiers::NONE,
    });
    assert!(
        matches!(
            state.handle_input(&click, &reg),
            InputOutcome::Action(Action::DashboardCloseShortcutsHelp)
        ),
        "clicking [✗] must request close",
    );
}

/// An inline-expand keypress toggles the selected row's id in and out of
/// `expanded_ids` (on→off→on) through the shared `toggle_membership`.
#[test]
fn shortcuts_modal_key_toggles_inline_expand() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let mut state = DashboardState::new();
    let reg = crate::actions::ActionRegistry::defaults();
    let entries = crate::views::shortcuts_help::build_entries(
        &[
            crate::actions::When::DashboardFocused,
            crate::actions::When::Always,
        ],
        &reg,
        true,
    );
    let picker = crate::views::shortcuts_help::build_initial_picker_state(&entries);
    let mut modal = Box::new(ShortcutsModalState {
        entries,
        state: picker,
        window: Default::default(),
        filter_active: false,
        collapsed_sections: Default::default(),
        expanded_ids: std::collections::HashSet::new(),
        mode: crate::views::shortcuts_help::ShortcutsHelpMode::Browse,
    });
    // Land on the first registry-backed hint (the section header is row 0).
    modal.state.selected = 1;
    state.shortcuts_modal = Some(modal);

    let right = || Event::Key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
    let expanded_len = |s: &DashboardState| s.shortcuts_modal.as_ref().unwrap().expanded_ids.len();

    assert!(matches!(
        state.handle_input(&right(), &reg),
        InputOutcome::Changed
    ));
    assert_eq!(expanded_len(&state), 1, "first press expands the row");
    state.handle_input(&right(), &reg);
    assert_eq!(expanded_len(&state), 0, "second press collapses it");
    state.handle_input(&right(), &reg);
    assert_eq!(expanded_len(&state), 1, "third press expands it again");
}

/// Opening the detail page and returning must leave every browse-list field
/// (selection, query, filter, collapsed, expanded) exactly as it was.
#[test]
fn shortcuts_modal_detail_round_trip_preserves_browse_state() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let mut state = DashboardState::new();
    let reg = crate::actions::ActionRegistry::defaults();
    let entries = crate::views::shortcuts_help::build_entries(
        &[
            crate::actions::When::DashboardFocused,
            crate::actions::When::Always,
        ],
        &reg,
        true,
    );
    let picker = crate::views::shortcuts_help::build_initial_picker_state(&entries);
    let mut modal = Box::new(ShortcutsModalState {
        entries,
        state: picker,
        window: Default::default(),
        filter_active: false,
        collapsed_sections: std::collections::HashSet::from([6usize]),
        expanded_ids: std::collections::HashSet::from([
            crate::views::shortcuts_help::ExpandKey::Action(crate::actions::ActionId::Quit),
        ]),
        mode: crate::views::shortcuts_help::ShortcutsHelpMode::Browse,
    });
    modal.state.selected = 1; // first registry-backed hint
    state.shortcuts_modal = Some(modal);

    let snapshot = |s: &DashboardState| {
        let m = s.shortcuts_modal.as_ref().unwrap();
        (
            m.state.selected,
            m.state.query().to_owned(),
            m.filter_active,
            m.collapsed_sections.clone(),
            m.expanded_ids.clone(),
        )
    };
    let before = snapshot(&state);

    let enter = Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    state.handle_input(&enter, &reg);
    assert!(
        state.shortcuts_modal.as_ref().unwrap().mode.is_detail(),
        "Enter on a registry hint opens the detail page",
    );

    let esc = Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    state.handle_input(&esc, &reg);
    assert!(
        state.shortcuts_modal.as_ref().unwrap().mode.is_browse(),
        "Esc returns to the browse list",
    );
    assert_eq!(
        before,
        snapshot(&state),
        "detail round-trip must preserve all browse-list state",
    );
}

/// Collapse/expand keypresses on a section header dismiss a pending
/// feedback toast like every other key (the invariant) —
/// the intercept must sit BELOW the handler's toast-clear tier.
#[test]
fn section_keys_clear_pending_toast() {
    let mut state = DashboardState::new();
    let reg = crate::actions::ActionRegistry::defaults();
    let key_sec = SectionKey::State(RowState::Working);
    state.focus_section(key_sec);
    state.set_error_toast("boom");
    let _ = state.handle_key(&KeyEvent::new(KeyCode::Left, KeyModifiers::NONE), &reg);
    assert!(state.is_section_collapsed(key_sec), "Left must collapse");
    assert!(
        state.error_toast.is_none(),
        "a collapse keypress must dismiss the pending toast",
    );
}

/// An armed delete confirmation is bound to the row that was selected
/// when `Ctrl+X` was pressed — any other key (nav included) must
/// disarm it, otherwise the footer's "press again to delete" hint
/// lingers while the cursor moves to other agents. The disarm must
/// NOT depend on `error_toast` (the Ctrl+X arm path plants none).
#[test]
fn nav_key_disarms_pending_delete_confirm() {
    let mut state = DashboardState::new();
    let reg = crate::actions::ActionRegistry::defaults();
    state.focus_row(DashboardRowId::TopLevel(AgentId(0)));
    state.arm_delete(DashboardRowId::TopLevel(AgentId(0)));
    assert!(state.error_toast.is_none(), "arm path plants no toast");
    let _ = state.handle_key(&KeyEvent::new(KeyCode::Down, KeyModifiers::NONE), &reg);
    assert!(
        state.delete_confirm.is_none(),
        "a nav keypress must disarm the pending delete confirm",
    );

    // Control — Ctrl+X itself preserves the armed confirm so the
    // dispatcher can observe it and delete.
    state.arm_delete(DashboardRowId::TopLevel(AgentId(0)));
    let _ = state.handle_key(
        &KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL),
        &reg,
    );
    assert!(
        state.delete_confirm.is_some(),
        "Ctrl+X must preserve the armed confirm for the dispatcher",
    );

    // The actual repro path: peek is open by default for a selected
    // row, and `handle_peek_key` CONSUMES Up/Down (agent switch) —
    // the disarm must sit above that intercept or nav keys never
    // reach it and the footer hint lingers.
    state.arm_delete(DashboardRowId::TopLevel(AgentId(0)));
    state.peek = Some(super::super::peek::PeekPanelState::new(
        DashboardRowId::TopLevel(AgentId(0)),
        peek_fields_for_test("Idle"),
    ));
    let _ = state.handle_key(&KeyEvent::new(KeyCode::Down, KeyModifiers::NONE), &reg);
    assert!(
        state.delete_confirm.is_none(),
        "a nav keypress consumed by the peek panel must still disarm the confirm",
    );
}

#[test]
fn click_delete_control_arms_then_confirms() {
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    let mut state = DashboardState::new();
    let id = DashboardRowId::TopLevel(AgentId(0));
    state
        .row_delete_rects
        .push((id.clone(), Rect::new(10, 2, 3, 1)));
    let click = |col, row| MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: col,
        row,
        modifiers: KeyModifiers::NONE,
    };
    // First `[✗]` click only arms — it must not open/attach the session.
    let first = state.handle_mouse(&click(11, 2));
    assert!(matches!(first, InputOutcome::Changed), "got {first:?}");
    assert!(!matches!(
        first,
        InputOutcome::Action(Action::DashboardAttach(_))
    ));
    assert_eq!(state.armed_delete_row_ref(), Some(&id));
    // Second click confirms.
    assert!(matches!(
        state.handle_mouse(&click(11, 2)),
        InputOutcome::Action(Action::DashboardDelete)
    ));
}

#[test]
fn focus_change_disarms_delete_confirm() {
    let mut state = DashboardState::new();
    let a = DashboardRowId::TopLevel(AgentId(0));
    let b = DashboardRowId::TopLevel(AgentId(1));
    state.focus_row(a.clone());
    state.arm_delete(a.clone());
    state.focus_row(a.clone());
    assert_eq!(state.armed_delete_row_ref(), Some(&a));
    state.focus_row(b);
    assert!(state.delete_confirm.is_none());
    state.arm_delete(DashboardRowId::TopLevel(AgentId(0)));
    state.focus_new_agent_button();
    assert!(state.delete_confirm.is_none());

    state.focus_row(a.clone());
    state.list_focused = true;
    state.arm_delete(a);
    state.dispatch_rect = Some(Rect::new(0, 10, 40, 1));
    let _ = state.handle_mouse(&crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
        column: 2,
        row: 10,
        modifiers: KeyModifiers::NONE,
    });
    assert!(state.delete_confirm.is_none());
    assert!(!state.list_focused);
}

/// An auto-repeat (held) Ctrl+X must not drive the destructive
/// arm→confirm: only discrete presses count, so holding the key can't
/// arm and immediately confirm a delete.
#[test]
fn ctrl_x_key_repeat_is_ignored() {
    let mut state = DashboardState::new();
    let reg = crate::actions::ActionRegistry::defaults();
    state.focus_row(DashboardRowId::TopLevel(AgentId(0)));
    let repeat = Event::Key(crossterm::event::KeyEvent {
        code: KeyCode::Char('x'),
        modifiers: KeyModifiers::CONTROL,
        kind: crossterm::event::KeyEventKind::Repeat,
        state: crossterm::event::KeyEventState::NONE,
    });
    assert!(matches!(
        state.handle_input(&repeat, &reg),
        InputOutcome::Unchanged
    ));
    // A real press still resolves to the stop action.
    let press = Event::Key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL));
    assert!(matches!(
        state.handle_input(&press, &reg),
        InputOutcome::Action(Action::DashboardStop)
    ));
}

/// Cmd+X is SUPER, not CONTROL. DashboardStop is bound to Ctrl+X only,
/// so a KKP Cmd+X must never arm/stop/delete — even with a highlight in
/// the dispatch box or peek reply (wack setups / Ghostty).
#[test]
fn cmd_x_does_not_stop_or_delete_on_dashboard() {
    let mut state = DashboardState::new();
    let reg = crate::actions::ActionRegistry::defaults();
    state.focus_row(DashboardRowId::TopLevel(AgentId(0)));
    state.dispatch.set_text("hello world");
    state.dispatch.textarea.set_selection(0, 5);
    let cmd_x = KeyEvent::new(KeyCode::Char('x'), KeyModifiers::SUPER);
    let outcome = state.handle_key(&cmd_x, &reg);
    assert!(
        !matches!(outcome, InputOutcome::Action(Action::DashboardStop)),
        "Cmd+X must not resolve to DashboardStop, got {outcome:?}",
    );
    assert!(
        state.delete_confirm.is_none(),
        "Cmd+X must not arm a delete confirm",
    );

    // Peek open (the default selected-row surface): still not stop.
    let mut peek = state_with_open_peek();
    peek.peek_reply.set_text("reply draft");
    peek.peek_reply.textarea.set_selection(0, 5);
    let peek_outcome = peek.handle_key(&cmd_x, &reg);
    assert!(
        !matches!(peek_outcome, InputOutcome::Action(Action::DashboardStop)),
        "Cmd+X with peek open must not stop, got {peek_outcome:?}",
    );
    assert!(peek.delete_confirm.is_none());
}

/// Ctrl+X still stops when the dispatch (or peek reply) has a highlight.
/// The new cut path must not steal the registry stop chord.
#[test]
fn ctrl_x_still_stops_with_a_prompt_highlight() {
    let mut state = DashboardState::new();
    let reg = crate::actions::ActionRegistry::defaults();
    state.focus_row(DashboardRowId::TopLevel(AgentId(0)));
    state.dispatch.set_text("hello world");
    state.dispatch.textarea.set_selection(0, 5);
    let ctrl_x = KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL);
    assert!(
        matches!(
            state.handle_key(&ctrl_x, &reg),
            InputOutcome::Action(Action::DashboardStop)
        ),
        "Ctrl+X must stay DashboardStop even with a dispatch highlight",
    );
    assert_eq!(
        state.dispatch.text(),
        "hello world",
        "Ctrl+X must not cut the dispatch highlight",
    );
    assert_eq!(state.dispatch.textarea.selection_range(), Some(0..5));

    let mut peek = state_with_open_peek();
    peek.peek_reply.set_text("reply draft");
    peek.peek_reply.textarea.set_selection(0, 5);
    assert!(
        matches!(
            peek.handle_key(&ctrl_x, &reg),
            InputOutcome::Action(Action::DashboardStop)
        ),
        "Ctrl+X with peek highlight must still stop",
    );
    assert_eq!(peek.peek_reply.text(), "reply draft");
}

/// `gc_stale_refs` dropping the selected row (session left the list)
/// must also disarm delete, so a later `y` can't delete a phantom row.
#[test]
fn gc_stale_refs_disarms_delete_when_selection_dropped() {
    let mut state = DashboardState::new();
    let a = DashboardRowId::TopLevel(AgentId(0));
    state.focus_row(a.clone());
    state.arm_delete(a.clone());
    assert!(state.armed_delete_row_ref().is_some());
    // The armed row is no longer alive → gc drops selection AND disarms.
    state.gc_stale_refs(&|_| false);
    assert!(state.selected.is_none());
    assert!(state.delete_confirm.is_none(), "stale arm must be cleared");
}

/// Section header selected while the LIST is focused — the input is
/// inactive, so Enter / Left / Right operate on the section even
/// when a draft is sitting in the (unfocused) dispatch input.
/// (With the input focused, text flips those keys to draft editing
/// / dispatch — covered by the gate's `prompt_empty` arm.)
#[test]
fn section_keys_work_with_draft_when_list_focused() {
    let mut state = DashboardState::new();
    let reg = crate::actions::ActionRegistry::defaults();
    let key_sec = SectionKey::State(RowState::Working);
    state.focus_section(key_sec);
    state.list_focused = true;
    state.dispatch.set_text("draft for a new agent");

    let _ = state.handle_key(&KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &reg);
    assert!(
        state.is_section_collapsed(key_sec),
        "list-focused Enter must toggle the section despite the draft",
    );
    let _ = state.handle_key(&KeyEvent::new(KeyCode::Right, KeyModifiers::NONE), &reg);
    assert!(
        !state.is_section_collapsed(key_sec),
        "list-focused Right must expand despite the draft",
    );
    let _ = state.handle_key(&KeyEvent::new(KeyCode::Left, KeyModifiers::NONE), &reg);
    assert!(
        state.is_section_collapsed(key_sec),
        "list-focused Left must collapse despite the draft",
    );
    assert_eq!(
        state.dispatch.text(),
        "draft for a new agent",
        "the inactive input's draft must survive untouched",
    );
}

/// vim `l` opens detail on list-focused rows; focused dispatch types `l`.
#[test]
fn vim_l_row_attach_and_input_focus() {
    use crate::app::actions::Action;
    use crate::views::dashboard::DashboardRowId;

    let reg = crate::actions::ActionRegistry::defaults();
    let id = DashboardRowId::TopLevel(crate::app::agent::AgentId(42));
    let l = KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE);

    crate::appearance::cache::set_vim_mode(true);
    let mut state = DashboardState::new();
    state.focus_row(id.clone());
    state.list_focused = true;
    match state.handle_key(&l, &reg) {
        InputOutcome::Action(Action::DashboardAttach(row)) => assert_eq!(row, id),
        other => panic!("list-focused vim `l` must attach, got {other:?}"),
    }

    let mut state = DashboardState::new();
    state.focus_row(id);
    state.list_focused = false;
    let outcome = state.handle_key(&l, &reg);
    crate::appearance::cache::set_vim_mode(false);
    assert!(
        !matches!(outcome, InputOutcome::Action(Action::DashboardAttach(_))),
        "input-focused vim `l` must not attach, got {outcome:?}",
    );
    assert_eq!(state.dispatch.text(), "l");
}

/// vim `l` on peek: unfocused attaches; focused empty reply types `l`.
#[test]
fn vim_l_peek_attach_and_focused_type() {
    use crate::app::actions::Action;
    let reg = crate::actions::ActionRegistry::defaults();
    let l = KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE);

    // state_with_open_peek pins vim off for seed focus; enable after.
    let mut state = state_with_open_peek();
    crate::appearance::cache::set_vim_mode(true);
    state.peek.as_mut().unwrap().focused = false;
    match state.handle_key(&l, &reg) {
        InputOutcome::Action(Action::DashboardAttach(row)) => {
            assert_eq!(row, DashboardRowId::TopLevel(AgentId(0)));
        }
        other => panic!("unfocused peek vim `l` must attach, got {other:?}"),
    }

    let mut state = state_with_open_peek();
    crate::appearance::cache::set_vim_mode(true);
    assert!(state.peek.as_ref().unwrap().focused);
    let outcome = state.handle_key(&l, &reg);
    let reply = state.peek_reply.text().to_string();
    crate::appearance::cache::set_vim_mode(false);
    assert!(
        !matches!(outcome, InputOutcome::Action(Action::DashboardAttach(_))),
        "focused reply vim `l` must not attach, got {outcome:?}",
    );
    assert_eq!(reply, "l");
}

/// List-focused vim `h`/`l` fold sections; input-focused or vim-off type.
#[test]
fn section_vim_hl_collapse_expand() {
    let reg = crate::actions::ActionRegistry::defaults();
    let key_sec = SectionKey::State(RowState::Working);

    // vim ON + LIST focused — `h`/`l` fold the section.
    crate::appearance::cache::set_vim_mode(true);
    let mut state = DashboardState::new();
    state.focus_section(key_sec);
    state.list_focused = true;
    let _ = state.handle_key(&KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE), &reg);
    let collapsed_after_h = state.is_section_collapsed(key_sec);
    let _ = state.handle_key(&KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE), &reg);
    let collapsed_after_l = state.is_section_collapsed(key_sec);
    // Reset before asserting so a failure can't leak vim state
    // into another test sharing this thread's cache.
    crate::appearance::cache::set_vim_mode(false);
    assert!(collapsed_after_h, "list-focused vim `h` must collapse");
    assert!(!collapsed_after_l, "list-focused vim `l` must expand");

    // vim ON + INPUT focused (list_focused == false) + empty draft —
    // `h`/`l` must type into the prompt, never fold the section.
    crate::appearance::cache::set_vim_mode(true);
    let mut state = DashboardState::new();
    state.focus_section(key_sec);
    // Input focused is the default; leave list_focused == false.
    let _ = state.handle_key(&KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE), &reg);
    let collapsed_after_input_h = state.is_section_collapsed(key_sec);
    let _ = state.handle_key(&KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE), &reg);
    let collapsed_after_input_l = state.is_section_collapsed(key_sec);
    let typed = state.dispatch.text().to_string();
    crate::appearance::cache::set_vim_mode(false);
    assert!(
        !collapsed_after_input_h && !collapsed_after_input_l,
        "input-focused vim `h`/`l` must not fold the section",
    );
    assert_eq!(
        typed, "hl",
        "input-focused vim `h`/`l` must type into the dispatch input",
    );

    // vim OFF — bare letters are dispatch-input edits, never
    // collapse keys, even with a section header selected.
    let mut state = DashboardState::new();
    state.focus_section(key_sec);
    let _ = state.handle_key(&KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE), &reg);
    assert!(
        !state.is_section_collapsed(key_sec),
        "vim-off `l` must not collapse",
    );
    assert_eq!(
        state.dispatch.text(),
        "l",
        "vim-off `l` must type into the dispatch input",
    );
}

/// Build a minimal top-level row for the reanchor tests.
fn reanchor_test_row(id: usize, state: RowState) -> super::super::row::DashboardRow {
    super::super::row::DashboardRow {
        id: DashboardRowId::TopLevel(AgentId(id)),
        label: format!("r{id}"),
        subtitle: None,
        state,
        activity: None,
        secondary_line: None,
        cwd_display: String::new(),
        cwd: std::path::PathBuf::from("/"),
        last_change_at: std::time::SystemTime::now(),
        pinned: false,
        is_active: false,
        badges: Vec::new(),
        context_pct: None,
        indent: 0,
        parent_label: None,
        is_more_placeholder: false,
        more_count: 0,
    }
}

/// A selected section header whose section no longer exists (row
/// churn removed its last row) is moved to the `[+ New Agent]`
/// button by `reanchor_selection`, so the footer hints and the
/// collapse keys never act on an invisible header.
#[test]
fn reanchor_moves_stale_section_cursor_to_button() {
    let mut state = DashboardState::new();
    state.focus_section(SectionKey::State(RowState::Idle));
    // Only a Working row remains — the Idle header is gone.
    let rows = vec![reanchor_test_row(1, RowState::Working)];
    state.reanchor_selection(&rows);
    assert!(
        state.selected_section.is_none(),
        "stale section cursor must be cleared",
    );
    assert!(
        state.new_agent_button_focused,
        "cursor must move to the [+ New Agent] button",
    );
}

/// A `s:state` filter suppresses ALL state headers — a section
/// cursor left over from before the filter must be re-anchored
/// even though the section's rows still exist.
#[test]
fn reanchor_moves_section_cursor_when_state_filter_hides_headers() {
    let mut state = DashboardState::new();
    state.focus_section(SectionKey::State(RowState::Working));
    state.filter = Filter::State(RowState::Working);
    let rows = vec![reanchor_test_row(1, RowState::Working)];
    state.reanchor_selection(&rows);
    assert!(
        state.selected_section.is_none(),
        "headers are suppressed under a state filter — the section cursor must clear",
    );
    assert!(state.new_agent_button_focused);
}

/// A section cursor whose header is still on screen is untouched.
#[test]
fn reanchor_keeps_live_section_cursor() {
    let mut state = DashboardState::new();
    state.focus_section(SectionKey::State(RowState::Working));
    let rows = vec![reanchor_test_row(1, RowState::Working)];
    state.reanchor_selection(&rows);
    assert_eq!(
        state.selected_section,
        Some(SectionKey::State(RowState::Working)),
        "a live section cursor must survive reanchoring",
    );
    assert!(!state.new_agent_button_focused);
}

/// A selected row that state churn migrated INTO a collapsed
/// section (still present in `rows`, but hidden by the collapse)
/// moves the cursor onto the section header that hid it — never an
/// invisible row with live footer hints / peek.
#[test]
fn reanchor_moves_collapse_hidden_row_cursor_to_its_header() {
    let mut state = DashboardState::new();
    state.set_section_collapsed(SectionKey::State(RowState::Working), true);
    // The row was selected while (say) Idle; it has since started
    // Working — and "Working" is collapsed.
    state.focus_row(DashboardRowId::TopLevel(AgentId(1)));
    let rows = vec![reanchor_test_row(1, RowState::Working)];
    state.reanchor_selection(&rows);
    assert!(
        state.selected.is_none(),
        "the hidden row must not keep the cursor",
    );
    assert_eq!(
        state.selected_section,
        Some(SectionKey::State(RowState::Working)),
        "the cursor must land on the header that hides the row",
    );
    assert!(!state.new_agent_button_focused);
}

/// Same churn scenario for the Pinned block: a selected pinned row
/// hidden by a collapsed "Pinned" section re-anchors to that header.
#[test]
fn reanchor_moves_collapse_hidden_pinned_row_to_pinned_header() {
    let mut state = DashboardState::new();
    state.set_section_collapsed(SectionKey::Pinned, true);
    state.focus_row(DashboardRowId::TopLevel(AgentId(1)));
    let mut row = reanchor_test_row(1, RowState::Idle);
    row.pinned = true;
    state.reanchor_selection(&[row]);
    assert!(state.selected.is_none());
    assert_eq!(
        state.selected_section,
        Some(SectionKey::Pinned),
        "a collapse-hidden pinned row must re-anchor to the Pinned header",
    );
}

/// A subagent row hidden by its PARENT's collapsed section
/// re-anchors to the parent's state header (subagents render under
/// the parent's group).
#[test]
fn reanchor_moves_collapse_hidden_subagent_to_parent_header() {
    let mut state = DashboardState::new();
    state.set_section_collapsed(SectionKey::State(RowState::Working), true);
    let child_id = DashboardRowId::Subagent {
        parent: AgentId(1),
        child_session_id: "child-1".into(),
    };
    state.focus_row(child_id.clone());
    let parent = reanchor_test_row(1, RowState::Working);
    let mut child = reanchor_test_row(2, RowState::Working);
    child.id = child_id;
    child.indent = 1;
    state.reanchor_selection(&[parent, child]);
    assert!(state.selected.is_none());
    assert_eq!(
        state.selected_section,
        Some(SectionKey::State(RowState::Working)),
        "a hidden subagent must re-anchor to its parent's header",
    );
}

/// A selected row stays selected when a DIFFERENT section is
/// collapsed (its own section is still expanded).
#[test]
fn reanchor_keeps_row_cursor_when_other_section_collapsed() {
    let mut state = DashboardState::new();
    state.set_section_collapsed(SectionKey::State(RowState::Idle), true);
    let sel = DashboardRowId::TopLevel(AgentId(1));
    state.focus_row(sel.clone());
    let rows = vec![
        reanchor_test_row(1, RowState::Working),
        reanchor_test_row(2, RowState::Idle),
    ];
    state.reanchor_selection(&rows);
    assert_eq!(
        state.selected,
        Some(sel),
        "a visible row's cursor must survive reanchoring",
    );
    assert!(state.selected_section.is_none());
}

/// Under a `s:state` filter headers are suppressed, so collapse
/// never hides rows — a leftover collapsed flag must NOT steal the
/// row cursor (the row is visible).
#[test]
fn reanchor_keeps_row_cursor_under_state_filter_despite_collapsed_flag() {
    let mut state = DashboardState::new();
    state.set_section_collapsed(SectionKey::State(RowState::Working), true);
    state.filter = Filter::State(RowState::Working);
    let sel = DashboardRowId::TopLevel(AgentId(1));
    state.focus_row(sel.clone());
    let rows = vec![reanchor_test_row(1, RowState::Working)];
    state.reanchor_selection(&rows);
    assert_eq!(
        state.selected,
        Some(sel),
        "headers (and thus collapse) are suppressed under a state filter — \
         the visible row must keep the cursor",
    );
    assert!(state.selected_section.is_none());
}

/// Clicking anywhere on the dispatch input box focuses the input
/// (clears `list_focused`). This must hold in vim mode too — there
/// the overview owns the keyboard (j/k nav), so a mouse user who
/// Tabbed or vim-navigated into the list would otherwise be stuck
/// with no way to click back into the prompt.
#[test]
fn click_on_dispatch_box_focuses_input_in_both_modes() {
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    use ratatui::layout::Rect;
    let box_rect = Rect {
        x: 0,
        y: 0,
        width: 80,
        height: 3,
    };
    let click = MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 10,
        row: 1,
        modifiers: crossterm::event::KeyModifiers::NONE,
    };
    for vim in [false, true] {
        crate::appearance::cache::set_vim_mode(vim);
        let mut state = DashboardState::new();
        // Overview focused (as if via Tab / vim nav).
        state.list_focused = true;
        // Box rect as recorded by `render_dashboard`.
        state.dispatch_rect = Some(box_rect);
        let outcome = state.handle_mouse(&click);
        let focused_input = !state.list_focused;
        // Reset before asserting so a failure can't leak vim state
        // into another test sharing this thread's cache.
        crate::appearance::cache::set_vim_mode(false);
        assert!(
            matches!(outcome, InputOutcome::Changed),
            "vim={vim}: click on dispatch box must report Changed, got {outcome:?}",
        );
        assert!(
            focused_input,
            "vim={vim}: click on dispatch box must focus the input (clear list_focused)",
        );
    }
}

/// A click that lands outside the dispatch box (with no row or
/// button underneath) does not steal focus into the input.
#[test]
fn click_outside_dispatch_box_leaves_focus() {
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    use ratatui::layout::Rect;
    let mut state = DashboardState::new();
    state.list_focused = true;
    // Box occupies the first 3 rows; click well below it.
    state.dispatch_rect = Some(Rect {
        x: 0,
        y: 0,
        width: 80,
        height: 3,
    });
    let click = MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 10,
        row: 20,
        modifiers: crossterm::event::KeyModifiers::NONE,
    };
    let outcome = state.handle_mouse(&click);
    assert!(
        matches!(outcome, InputOutcome::Unchanged),
        "click below the dispatch box must be a no-op, got {outcome:?}",
    );
    assert!(
        state.list_focused,
        "click outside the dispatch box must not move focus to the input",
    );
}

/// Registry walk — `Ctrl+\\` is reserved for dashboard
/// navigation. It's bound to `OpenDashboard` (global, `When::Always`) and
/// to `DashboardOverlayExit` (`When::DashboardOverlay`) — disjoint
/// contexts that both route to the dashboard (the overlay intercept maps
/// both to `DashboardOverlayExit`). No OTHER, unrelated action may claim
/// it. Notably it must NOT be the worktree toggle's key (that's Ctrl+W).
#[test]
fn ctrl_backslash_only_bound_to_dashboard_navigation() {
    use crate::actions::ActionId;
    let reg = crate::actions::ActionRegistry::defaults();
    let mut bound_to_ctrl_backslash: Vec<ActionId> = Vec::new();
    for def in reg.all() {
        let mut keys = vec![def.default_key];
        keys.extend_from_slice(&def.alt_keys);
        for k in keys {
            let s = k.display().to_string();
            if s.eq_ignore_ascii_case("Ctrl+\\") {
                bound_to_ctrl_backslash.push(def.id);
            }
        }
    }
    bound_to_ctrl_backslash.sort_by_key(|id| format!("{id:?}"));
    assert_eq!(
        bound_to_ctrl_backslash,
        vec![ActionId::DashboardOverlayExit, ActionId::OpenDashboard],
        "Ctrl+\\ must be bound only to the two dashboard-navigation actions",
    );
}

/// Direct unit test for `clamp_viewport`: when the
/// total row count shrinks below the current offset, the offset
/// is pulled in to keep `max_offset = total - viewport_h`.
#[test]
fn clamp_viewport_pulls_offset_when_rows_shrink() {
    let mut s = DashboardState::new();
    s.viewport_offset = 10;
    // No selection, viewport=5, total=8 → max_offset = 3.
    s.clamp_viewport(None, 5, 8);
    assert_eq!(s.viewport_offset, 3);
}

/// Direct unit test for `clamp_viewport`: a selection
/// below the visible window snaps the offset down so the row is
/// visible at the bottom edge.
#[test]
fn clamp_viewport_snaps_to_selection() {
    let mut s = DashboardState::new();
    s.viewport_offset = 0;
    // Selection at line 12, viewport=5, total=20.
    // 12 >= offset + 5 (0 + 5) → offset = 12 + 1 - 5 = 8.
    s.clamp_viewport(Some(12), 5, 20);
    assert_eq!(s.viewport_offset, 8);
}

/// Selection above the window pulls the offset up to
/// keep the selection visible at the top edge.
#[test]
fn clamp_viewport_snaps_offset_up_when_selection_scrolls_above() {
    let mut s = DashboardState::new();
    s.viewport_offset = 10;
    s.clamp_viewport(Some(2), 5, 20);
    // sel_idx < offset → offset = sel_idx (2).
    assert_eq!(s.viewport_offset, 2);
}

/// zero-height viewport: clamp returns 0 (no visible
/// rows means nothing to scroll to).
#[test]
fn clamp_viewport_handles_zero_viewport_height() {
    let mut s = DashboardState::new();
    s.viewport_offset = 5;
    s.clamp_viewport(Some(3), 0, 10);
    // viewport_h = 0 → no snap-to-selection; max_offset = total -
    // 0 = 10, so offset stays at 5.
    assert_eq!(s.viewport_offset, 5);
}

// -----------------------------------------------------------------
// Mouse wheel decoupled from selection
// -----------------------------------------------------------------

/// `handle_scroll` flags `manual_scroll_active` so the next
/// `clamp_viewport` knows to skip the snap-to-selection
/// pull-back.
#[test]
fn handle_scroll_sets_manual_scroll_active() {
    let mut s = DashboardState::new();
    assert!(!s.manual_scroll_active);
    s.handle_scroll(3);
    assert!(s.manual_scroll_active);
    assert_eq!(s.viewport_offset, 3);
}

/// `handle_scroll(0)` is a no-op — neither the offset nor the
/// flag changes. Without this, a stray zero-line accumulator
/// flush would clobber the snap state.
#[test]
fn handle_scroll_zero_lines_is_noop() {
    let mut s = DashboardState::new();
    s.viewport_offset = 4;
    s.handle_scroll(0);
    assert!(!s.manual_scroll_active);
    assert_eq!(s.viewport_offset, 4);
}

/// Negative scrolls (wheel up) move the offset toward the top
/// AND still flag the viewport as user-driven.
#[test]
fn handle_scroll_negative_moves_offset_up() {
    let mut s = DashboardState::new();
    s.viewport_offset = 10;
    s.handle_scroll(-4);
    assert!(s.manual_scroll_active);
    assert_eq!(s.viewport_offset, 6);
}

/// Saturating arithmetic guards the upper-edge: scrolling up
/// when already at 0 stays at 0 (no underflow panic) and still
/// flips the manual-scroll flag.
#[test]
fn handle_scroll_saturates_at_zero() {
    let mut s = DashboardState::new();
    s.viewport_offset = 2;
    s.handle_scroll(-99);
    assert_eq!(s.viewport_offset, 0);
    assert!(s.manual_scroll_active);
}

/// THE FIX: when the user has manually scrolled, the
/// snap-to-selection in `clamp_viewport` is skipped so the
/// viewport doesn't get yanked back to the selected row. Without
/// this skip, scrolling past the cursor was a no-op visually
/// (the renderer snapped it back next frame).
#[test]
fn clamp_viewport_skips_snap_when_manual_scroll_active() {
    let mut s = DashboardState::new();
    s.manual_scroll_active = true;
    // viewport_h=5, total=50, selection at line 0 — without the
    // skip, the snap would pull offset back to 0.
    s.viewport_offset = 20;
    s.clamp_viewport(Some(0), 5, 50);
    assert_eq!(
        s.viewport_offset, 20,
        "manual_scroll_active must suppress the snap-to-selection pull-back",
    );
}

/// The manual-scroll flag does NOT disable the bounds clamp —
/// scrolling past the bottom edge still stops at `max_offset`.
/// Otherwise wheel acceleration would let the user park the
/// viewport on an entirely empty band below the last row.
#[test]
fn clamp_viewport_still_clamps_max_offset_when_manual_scroll_active() {
    let mut s = DashboardState::new();
    s.manual_scroll_active = true;
    s.viewport_offset = 100;
    s.clamp_viewport(Some(0), 5, 20);
    // max_offset = 20 - 5 = 15.
    assert_eq!(s.viewport_offset, 15);
}

/// Clearing the manual-scroll flag re-engages the
/// snap-to-selection on the next clamp, restoring the keyboard-
/// nav contract.
#[test]
fn clear_manual_scroll_re_engages_snap_to_selection() {
    let mut s = DashboardState::new();
    s.manual_scroll_active = true;
    s.viewport_offset = 20;
    s.clear_manual_scroll();
    assert!(!s.manual_scroll_active);
    s.clamp_viewport(Some(0), 5, 50);
    // With the flag cleared, the snap pulls the offset back so
    // selection at line 0 is visible.
    assert_eq!(s.viewport_offset, 0);
}

/// Env var force-disables.
///
/// Guard the env-var mutation with `serial_test`'s
/// per-key serial lock. The `ECHO_AGENT_AGENT_DASHBOARD` key means this
/// test runs serially with any other test that decorates itself
/// with `#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]` — see the
/// `dispatch_open_dashboard`-calling tests in `app::dispatch`.
/// A function-local `Mutex` would only serialize
/// against itself; readers in other tests
/// could still observe the transient `0` value.
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn env_var_force_disables() {
    // SAFETY: the test temporarily mutates a process-wide env var.
    // `serial_test`'s lock ensures no other test marked with the
    // same `ECHO_AGENT_AGENT_DASHBOARD` key reads it concurrently.
    unsafe { std::env::set_var("ECHO_AGENT_AGENT_DASHBOARD", "0") };
    assert!(!super::super::dashboard_enabled());
    unsafe { std::env::remove_var("ECHO_AGENT_AGENT_DASHBOARD") };
}

// ── Location picker ─────────────────────────────────────────────

fn location_candidate(path: &str, label: &str) -> LocationCandidate {
    LocationCandidate {
        path: PathBuf::from(path),
        label: label.to_string(),
        detail: path.to_string(),
        worktree: None,
    }
}

/// Build a picker over `recents` with a fixed base cwd and no worktrees.
fn location_picker(recents: Vec<LocationCandidate>) -> LocationPickerState {
    LocationPickerState::new(
        recents,
        PathBuf::from("/base"),
        std::collections::HashMap::new(),
    )
}

/// Build a picker with a worktree index for tagging suggestions.
fn location_picker_with_worktrees(
    recents: Vec<LocationCandidate>,
    worktrees: std::collections::HashMap<PathBuf, String>,
) -> LocationPickerState {
    LocationPickerState::new(recents, PathBuf::from("/base"), worktrees)
}

fn visible_labels(lp: &LocationPickerState) -> Vec<String> {
    lp.visible_candidates()
        .into_iter()
        .map(|c| c.label)
        .collect()
}

#[test]
fn location_visible_empty_query_shows_all_recents() {
    let lp = location_picker(vec![
        location_candidate("/home/me/alpha", "alpha"),
        location_candidate("/home/me/beta", "beta"),
    ]);
    assert_eq!(visible_labels(&lp), vec!["alpha", "beta"]);
}

#[test]
fn location_visible_filters_recents_by_substring() {
    let mut lp = location_picker(vec![
        location_candidate("/home/me/alpha", "alpha"),
        location_candidate("/home/me/beta", "beta"),
    ]);
    lp.picker.set_query("bet");
    assert_eq!(visible_labels(&lp), vec!["beta"]);
}

#[test]
fn location_query_is_path_detection() {
    let mut lp = location_picker(vec![]);
    for q in ["/abs", "~/x", "rel/sub", "~"] {
        lp.picker.set_query(q);
        assert!(lp.query_is_path(), "`{q}` should be path mode");
    }
    for q in ["", "alpha", "bet"] {
        lp.picker.set_query(q);
        assert!(!lp.query_is_path(), "`{q}` should be recents mode");
    }
}

/// Windows drive-prefix detection is platform-independent (the
/// `cfg!(windows)` gate on its use is exercised only on Windows, but the
/// predicate itself must be correct everywhere).
#[test]
fn windows_drive_prefix_detection() {
    assert!(has_windows_drive_prefix("C:\\Users\\me"));
    assert!(has_windows_drive_prefix("d:/projects"));
    assert!(has_windows_drive_prefix("Z:"));
    assert!(!has_windows_drive_prefix("/usr/local"));
    assert!(!has_windows_drive_prefix("~/x"));
    assert!(!has_windows_drive_prefix("C"));
    assert!(!has_windows_drive_prefix("1:\\nope"));
}

#[test]
fn location_chosen_input_falls_back_to_typed_path() {
    let mut lp = location_picker(vec![location_candidate("/home/me/alpha", "alpha")]);
    // A path with no matching suggestion → the raw typed path is used.
    lp.picker.set_query("/no/such/dir");
    assert_eq!(lp.chosen_input().as_deref(), Some("/no/such/dir"));
}

#[test]
fn location_chosen_input_uses_selected_recent() {
    let mut lp = location_picker(vec![
        location_candidate("/home/me/alpha", "alpha"),
        location_candidate("/home/me/beta", "beta"),
    ]);
    lp.picker.selected = 1;
    assert_eq!(lp.chosen_input().as_deref(), Some("/home/me/beta"));
}

#[test]
fn location_path_completion_lists_filters_and_hides_dotdirs() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir(tmp.path().join("alpha")).unwrap();
    std::fs::create_dir(tmp.path().join("beta")).unwrap();
    std::fs::create_dir(tmp.path().join(".hidden")).unwrap();
    let mut lp = location_picker(vec![]);

    // Trailing slash → list the (non-hidden) subdirs.
    lp.picker.set_query(format!("{}/", tmp.path().display()));
    lp.refresh_suggestions();
    let labels = visible_labels(&lp);
    assert!(labels.contains(&"alpha".to_string()), "got: {labels:?}");
    assert!(labels.contains(&"beta".to_string()), "got: {labels:?}");
    assert!(
        !labels.contains(&".hidden".to_string()),
        "dotdirs hidden unless the partial starts with `.`, got: {labels:?}",
    );

    // Prefix filter on the final segment.
    lp.picker.set_query(format!("{}/al", tmp.path().display()));
    lp.refresh_suggestions();
    assert_eq!(visible_labels(&lp), vec!["alpha"]);

    // A leading dot in the partial reveals dot-directories.
    lp.picker.set_query(format!("{}/.h", tmp.path().display()));
    lp.refresh_suggestions();
    assert_eq!(visible_labels(&lp), vec![".hidden"]);
}

#[test]
fn location_path_completion_tags_worktree_subdirs() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir(tmp.path().join("wt")).unwrap();
    std::fs::create_dir(tmp.path().join("plain")).unwrap();
    // Index `wt` as a managed worktree (keys are canonical paths).
    let canon = dunce::canonicalize(tmp.path()).unwrap_or_else(|_| tmp.path().to_path_buf());
    let mut worktrees = std::collections::HashMap::new();
    worktrees.insert(canon.join("wt"), "my-feature".to_string());

    let mut lp = location_picker_with_worktrees(vec![], worktrees);
    lp.picker.set_query(format!("{}/", tmp.path().display()));
    lp.refresh_suggestions();

    let visible = lp.visible_candidates();
    let wt = visible.iter().find(|c| c.label == "wt").expect("wt listed");
    assert_eq!(wt.worktree.as_deref(), Some("my-feature"));
    let plain = visible
        .iter()
        .find(|c| c.label == "plain")
        .expect("plain listed");
    assert_eq!(plain.worktree, None);
}

/// A worktree directory that is itself a symlink still gets tagged:
/// the index key is the canonical (real) path, so `read_subdirs` must
/// canonicalize the entry (resolving the symlink), not just join the
/// name to the canonical parent.
#[cfg(unix)]
#[test]
fn location_path_completion_tags_symlinked_worktree() {
    let real = tempfile::tempdir().unwrap(); // the real worktree target
    let parent = tempfile::tempdir().unwrap(); // the dir we list
    std::os::unix::fs::symlink(real.path(), parent.path().join("link")).unwrap();

    // Index keyed by the real (canonical) path, as the worktree DB is.
    let real_canon = dunce::canonicalize(real.path()).unwrap();
    let mut worktrees = std::collections::HashMap::new();
    worktrees.insert(real_canon, "linked-wt".to_string());

    let mut lp = location_picker_with_worktrees(vec![], worktrees);
    lp.picker.set_query(format!("{}/", parent.path().display()));
    lp.refresh_suggestions();

    let link = lp
        .visible_candidates()
        .into_iter()
        .find(|c| c.label == "link")
        .expect("symlinked dir listed");
    assert_eq!(link.worktree.as_deref(), Some("linked-wt"));
}

#[test]
fn click_on_location_label_opens_picker() {
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    let mut state = DashboardState::new();
    state.location_hit.set(Some(Rect {
        x: 0,
        y: 0,
        width: 20,
        height: 1,
    }));
    let click = MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 3,
        row: 0,
        modifiers: KeyModifiers::NONE,
    };
    assert!(matches!(
        state.handle_mouse(&click),
        InputOutcome::Action(Action::DashboardOpenLocationPicker)
    ));
}

#[test]
fn ctrl_l_opens_location_picker() {
    let mut state = DashboardState::new();
    let reg = crate::actions::ActionRegistry::defaults();
    let key = KeyEvent::new(KeyCode::Char('l'), KeyModifiers::CONTROL);
    assert!(matches!(
        state.handle_input(&Event::Key(key), &reg),
        InputOutcome::Action(Action::DashboardOpenLocationPicker)
    ));
}

#[test]
fn location_picker_esc_closes() {
    // Pin vim-mode off; this test asserts the non-vim picker path.
    crate::appearance::cache::set_vim_mode(false);
    let mut state = DashboardState::new();
    state.location_picker = Some(location_picker(vec![location_candidate("/tmp", "tmp")]));
    let reg = crate::actions::ActionRegistry::defaults();
    let esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
    assert!(matches!(
        state.handle_input(&Event::Key(esc), &reg),
        InputOutcome::Action(Action::DashboardCloseLocationPicker)
    ));
}

/// Editing the path clears a stale "Not a directory" error so it
/// doesn't linger next to a corrected (possibly valid) input.
#[test]
fn location_picker_edit_clears_error() {
    let mut state = DashboardState::new();
    let mut lp = location_picker(vec![]);
    lp.error = Some("Not a directory: /bad".to_string());
    state.location_picker = Some(lp);
    let reg = crate::actions::ActionRegistry::defaults();

    // Typing a character edits the query → the error is dropped.
    let key = KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE);
    let _ = state.handle_input(&Event::Key(key), &reg);
    assert!(
        state.location_picker.as_ref().unwrap().error.is_none(),
        "editing the path must clear the stale error",
    );
}

#[test]
fn location_picker_enter_selects_recent() {
    let mut state = DashboardState::new();
    state.location_picker = Some(location_picker(vec![
        location_candidate("/home/me/alpha", "alpha"),
        location_candidate("/home/me/beta", "beta"),
    ]));
    state.location_picker.as_mut().unwrap().picker.selected = 1;
    let reg = crate::actions::ActionRegistry::defaults();
    let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
    match state.handle_input(&Event::Key(enter), &reg) {
        InputOutcome::Action(Action::DashboardChangeLocation { input }) => {
            assert_eq!(input, "/home/me/beta");
        }
        other => panic!("expected DashboardChangeLocation, got {other:?}"),
    }
}

#[test]
fn location_picker_tab_fills_selected_path() {
    let mut state = DashboardState::new();
    state.location_picker = Some(location_picker(vec![
        // Paths outside $HOME so `display_path` leaves them absolute,
        // keeping the assertion independent of the test machine's home.
        location_candidate("/opt/projects/alpha", "alpha"),
        location_candidate("/opt/projects/beta", "beta"),
    ]));
    state.location_picker.as_mut().unwrap().picker.selected = 1;
    let reg = crate::actions::ActionRegistry::defaults();
    let tab = KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE);
    assert!(matches!(
        state.handle_input(&Event::Key(tab), &reg),
        InputOutcome::Changed
    ));
    let lp = state.location_picker.as_ref().unwrap();
    assert_eq!(lp.picker.query(), "/opt/projects/beta/");
    assert_eq!(lp.picker.query_cursor(), lp.picker.query().len());
}

#[test]
fn location_path_completion_enter_uses_selected_subdir() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir(tmp.path().join("alpha")).unwrap();
    let mut lp = location_picker(vec![]);
    lp.picker.set_query(format!("{}/al", tmp.path().display()));
    lp.refresh_suggestions();

    let mut state = DashboardState::new();
    state.location_picker = Some(lp);
    let reg = crate::actions::ActionRegistry::defaults();
    let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
    match state.handle_input(&Event::Key(enter), &reg) {
        InputOutcome::Action(Action::DashboardChangeLocation { input }) => {
            let expected = tmp.path().join("alpha").to_string_lossy().into_owned();
            assert_eq!(input, expected);
        }
        other => panic!("expected DashboardChangeLocation, got {other:?}"),
    }
}

#[test]
fn location_picker_typed_path_no_match_uses_raw_query() {
    let mut state = DashboardState::new();
    state.location_picker = Some(location_picker(vec![location_candidate(
        "/home/me/alpha",
        "alpha",
    )]));
    let reg = crate::actions::ActionRegistry::defaults();
    // A guaranteed-absent home path → no suggestion → the raw query is used.
    for c in "~/__nope_zzz__".chars() {
        let key = KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);
        let _ = state.handle_input(&Event::Key(key), &reg);
    }
    let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
    match state.handle_input(&Event::Key(enter), &reg) {
        InputOutcome::Action(Action::DashboardChangeLocation { input }) => {
            assert_eq!(input, "~/__nope_zzz__");
        }
        other => panic!("expected DashboardChangeLocation, got {other:?}"),
    }
}

fn lease_fixture_agent() -> (
    AgentId,
    indexmap::IndexMap<AgentId, crate::app::agent_view::AgentView>,
) {
    use crate::scrollback::block::RenderBlock;
    let id = AgentId(1);
    let mut agent = crate::test_util::make_agent_view(Some("s1"), "/tmp");
    agent.scrollback.push_block(RenderBlock::user_prompt("one"));
    agent
        .scrollback
        .push_block(RenderBlock::agent_message("long response body for wrap"));
    agent.scrollback.push_block(RenderBlock::user_prompt("two"));
    agent
        .scrollback
        .push_block(RenderBlock::agent_message("second reply"));
    agent.scrollback.prepare_layout(80, 24);
    agent.scrollback.set_selected(Some(0));
    agent.scrollback.set_scroll_offset(2);
    let mut agents = indexmap::IndexMap::new();
    agents.insert(id, agent);
    (id, agents)
}

#[test]
fn peek_viewport_lease_restore_without_page_flip_keeps_pre_guest_nav() {
    let (id, mut agents) = lease_fixture_agent();
    let pre = agents[&id].scrollback.capture_viewport_snapshot();
    let mut dash = DashboardState::new();
    let row = DashboardRowId::TopLevel(id);
    dash.begin_peek_viewport(row, &mut agents);
    assert!(dash.peek_viewport.is_some());
    assert!(agents[&id].scrollback.is_follow_mode());
    assert!(
        agents
            .get_mut(&id)
            .unwrap()
            .scrollback
            .prepare_layout(40, 6),
        "guest width change is Case 1"
    );

    dash.restore_peek_viewport(&mut agents);
    assert!(dash.peek_viewport.is_none());
    let sb = &mut agents.get_mut(&id).unwrap().scrollback;
    assert_eq!(sb.scroll_offset(), pre.scroll_offset);
    assert_eq!(sb.is_follow_mode(), pre.follow_mode);
    assert_eq!(sb.selected(), pre.selected);
    assert!(
        sb.prepare_layout(80, 24),
        "restore must invalidate so full-width prepare is Case 1"
    );
    let snap = sb.capture_viewport_snapshot();
    assert_eq!(snap.last_width, 80);
}

#[test]
fn peek_viewport_lease_page_flip_re_pins_entry_on_restore() {
    let (id, mut agents) = lease_fixture_agent();
    let mut dash = DashboardState::new();
    dash.begin_peek_viewport(DashboardRowId::TopLevel(id), &mut agents);
    let page_flip_entry = {
        let sb = &mut agents.get_mut(&id).unwrap().scrollback;
        sb.prepare_layout(40, 6);
        let last = sb.len().saturating_sub(1);
        let entry_id = sb.entry(last).unwrap().id;
        sb.set_selected(Some(last));
        sb.scroll_to_entry_top(last);
        sb.enable_follow_with_preserve();
        entry_id
    };
    assert!(agents[&id].scrollback.is_follow_preserve_scroll());
    dash.note_page_flip_for_lease(id, page_flip_entry, &agents);
    assert_eq!(
        dash.peek_viewport.as_ref().and_then(|l| l.page_flip_entry),
        Some(page_flip_entry)
    );

    dash.restore_peek_viewport(&mut agents);
    let sb = &agents[&id].scrollback;
    assert!(sb.is_follow_mode());
    assert!(sb.is_follow_preserve_scroll());
    assert_eq!(sb.selected(), Some(sb.len().saturating_sub(1)));
    let snap = sb.capture_viewport_snapshot();
    assert_eq!(snap.last_width, 80);
}

#[test]
fn set_peek_none_does_not_clear_viewport_lease() {
    let (id, mut agents) = lease_fixture_agent();
    let mut dash = DashboardState::new();
    dash.begin_peek_viewport(DashboardRowId::TopLevel(id), &mut agents);
    assert!(dash.peek_viewport.is_some());
    dash.set_peek(None);
    assert!(dash.peek_viewport.is_some());
    dash.restore_peek_viewport(&mut agents);
    assert!(dash.peek_viewport.is_none());
}

#[test]
fn sticky_begin_peek_does_not_recapture() {
    let (id, mut agents) = lease_fixture_agent();
    let mut dash = DashboardState::new();
    dash.begin_peek_viewport(DashboardRowId::TopLevel(id), &mut agents);
    let snap_offset = dash
        .peek_viewport
        .as_ref()
        .map(|l| l.snapshot.scroll_offset)
        .unwrap();
    agents.get_mut(&id).unwrap().scrollback.set_scroll_offset(0);
    dash.begin_peek_viewport(DashboardRowId::TopLevel(id), &mut agents);
    assert_eq!(
        dash.peek_viewport.as_ref().unwrap().snapshot.scroll_offset,
        snap_offset
    );
}

#[test]
fn note_page_flip_only_when_row_and_entry_match() {
    let (id, mut agents) = lease_fixture_agent();
    let mut dash = DashboardState::new();
    dash.begin_peek_viewport(DashboardRowId::TopLevel(id), &mut agents);
    let entry_id = agents[&id].scrollback.entry(3).unwrap().id;
    agents
        .get_mut(&id)
        .unwrap()
        .scrollback
        .enable_follow_with_preserve();
    dash.note_page_flip_for_lease(AgentId(99), entry_id, &agents);
    assert!(
        dash.peek_viewport
            .as_ref()
            .unwrap()
            .page_flip_entry
            .is_none()
    );
    dash.note_page_flip_for_lease(id, crate::scrollback::EntryId::new(u64::MAX), &agents);
    assert!(
        dash.peek_viewport
            .as_ref()
            .unwrap()
            .page_flip_entry
            .is_none()
    );
    dash.note_page_flip_for_lease(id, entry_id, &agents);
    let lease = dash.peek_viewport.as_ref().unwrap();
    assert_eq!(lease.page_flip_entry, Some(entry_id));
    assert!(!lease.snapshot.follow_preserve_scroll);
    assert_eq!(lease.snapshot.selected, Some(0));
}

#[test]
fn restore_ignores_page_flip_entry_removed_during_lease() {
    let (id, mut agents) = lease_fixture_agent();
    let pre = agents[&id].scrollback.capture_viewport_snapshot();
    let mut dash = DashboardState::new();
    dash.begin_peek_viewport(DashboardRowId::TopLevel(id), &mut agents);
    let entry_id = agents[&id].scrollback.entry(2).unwrap().id;
    agents
        .get_mut(&id)
        .unwrap()
        .scrollback
        .enable_follow_with_preserve();
    dash.note_page_flip_for_lease(id, entry_id, &agents);
    agents
        .get_mut(&id)
        .unwrap()
        .scrollback
        .remove_entry(entry_id);

    dash.restore_peek_viewport(&mut agents);

    assert!(dash.peek_viewport.is_none());
    assert_eq!(agents[&id].scrollback.selected(), pre.selected);
    assert_eq!(agents[&id].scrollback.is_follow_mode(), pre.follow_mode);
}

#[test]
fn note_page_flip_ignores_subagent_lease_on_parent_agent() {
    let (id, mut agents) = lease_fixture_agent();
    let child = crate::test_util::make_agent_view(Some("child"), "/tmp");
    agents
        .get_mut(&id)
        .unwrap()
        .subagent_views
        .insert("child".into(), Box::new(child));
    let mut dash = DashboardState::new();
    dash.begin_peek_viewport(
        DashboardRowId::Subagent {
            parent: id,
            child_session_id: "child".into(),
        },
        &mut agents,
    );
    let entry_id = agents[&id].scrollback.entry(3).unwrap().id;
    dash.note_page_flip_for_lease(id, entry_id, &agents);
    assert!(
        dash.peek_viewport
            .as_ref()
            .unwrap()
            .page_flip_entry
            .is_none(),
        "parent drain must not write parent entries onto a subagent lease"
    );
    agents
        .get_mut(&id)
        .unwrap()
        .scrollback
        .enable_follow_with_preserve();
    dash.note_page_flip_for_lease(id, entry_id, &agents);
    assert!(
        dash.peek_viewport
            .as_ref()
            .unwrap()
            .page_flip_entry
            .is_none()
    );
}
