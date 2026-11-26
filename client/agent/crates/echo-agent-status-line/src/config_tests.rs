//! Some cases are written in TOML rather than JSON: `#[serde(untagged)]` replays
//! a buffered value through a fresh deserializer, and that replay behaves in a
//! format-dependent way, so the format a user writes needs its own coverage.

use serde_json::json;

use super::test_support::StatusLineConfigFixture;
use super::*;

/// Stands in for the `[ui]` table, which lives downstream. `theme` is a
/// sibling key the section must not take down with it.
#[derive(Default, Deserialize)]
#[serde(default)]
struct UiTable {
    theme: Option<String>,
    status_line: StatusLineConfig,
}

const THEME: &str = "kanagawa";
const SURVIVES: &str = "[ui] must survive whatever the status line says";

fn ui(section: &str) -> UiTable {
    let json = format!(r#"{{"theme": "{THEME}", "status_line": {section}}}"#);
    serde_json::from_str(&json).expect(SURVIVES)
}

fn ui_toml(section: &str) -> UiTable {
    toml::from_str(&format!("theme = \"{THEME}\"\n{section}")).expect(SURVIVES)
}

#[track_caller]
fn names_the_problem(ui: UiTable, expect: &str, input: &str) {
    assert_eq!(ui.theme.as_deref(), Some(THEME), "{input}");
    let problem = ui.status_line.problem();
    assert!(
        problem.is_some_and(|problem| problem.contains(expect)),
        "{input} reported {problem:?}, which does not name {expect}"
    );
}

#[test]
fn parses_the_vocabulary_a_user_writes() {
    let from_json = ui(r#"{"type": "builtin", "items": ["cwd", "turn-timer"], "padding": 2}"#);
    let from_toml = ui_toml(
        "[status_line]\ntype = \"builtin\"\nitems = [\"cwd\", \"turn-timer\"]\npadding = 2\n",
    );
    let items = &[StatusLineItem::Cwd, StatusLineItem::TurnTimer];

    for ui in [from_json, from_toml] {
        let section = &ui.status_line;
        assert_eq!(ui.theme.as_deref(), Some(THEME));
        assert_eq!(section.kind, Some(StatusLineType::Builtin));
        assert_eq!(section.effective_items(), items);
        assert_eq!(section.padding(), 2);
        assert!(section.problem().is_none());
    }
}

#[test]
fn value_it_cannot_read_is_named_and_the_ui_table_survives() {
    for (section, expect) in [
        (r#"{"type": "enabled"}"#, r#"type = "enabled""#),
        (r#"{"type": 7}"#, "ignored type"),
        (r#"{"items": ["cwd", "brnach"]}"#, r#"items = "brnach""#),
        (r#"{"items": "cwd"}"#, "ignored items"),
        (r#"{"padding": "2"}"#, "ignored padding"),
        (r#"{"padding": 70000}"#, "ignored padding"),
        (r#"{"refresh_interval": "5m"}"#, "ignored refresh_interval"),
        (r#""builtin""#, "must be a table"),
        (r#"{"command": "~/status_line.sh"}"#, "needs type"),
    ] {
        names_the_problem(ui(section), expect, section);
    }

    for (section, expect) in [
        ("[status_line]\ntype = \"buitlin\"\n", "type = \"buitlin\""),
        ("status_line = \"builtin\"\n", "must be a table"),
        ("[status_line]\npadding = 2\n", "needs type"),
    ] {
        names_the_problem(ui_toml(section), expect, section);
    }

    let partial =
        ui(r#"{"type": "builtin", "items": ["cwd", "brnach"], "padding": "2"}"#).status_line;
    assert_eq!(partial.effective_items(), &[StatusLineItem::Cwd]);
    assert_eq!(partial.padding, None, "a value we could not read is unset");
}

#[test]
fn unknown_key_is_named_rather_than_silently_dropped() {
    let section = "[status_line]\ntype = \"command\"\ncommand = \"x\"\ncolour = \"red\"\n";
    let named = ui_toml(section).status_line;
    assert_eq!(named.unknown_keys, ["colour"]);
    assert_eq!(
        named.resolve(),
        Some(ResolvedStatusLine::Command { command: "x" })
    );
    assert!(
        named.problem().is_none(),
        "an unknown key is a warning, not a message to paint over the row"
    );

    let alone = ui_toml("[status_line]\ncolour = \"red\"\n").status_line;
    assert!(
        !alone.reserves_a_row(),
        "an unknown key cannot switch on a row nobody asked for"
    );
}

#[test]
fn typo_cannot_switch_a_row_back_on_after_the_user_switched_it_off() {
    let off = ui(r#"{"type": "disabled", "padding": "2"}"#).status_line;
    assert!(off.problem().is_some(), "the typo is still reported");
    assert!(off.problem_to_paint().is_none() && !off.reserves_a_row());

    let stray = ui(r#"{"type": "disabled", "command": "~/x.sh"}"#).status_line;
    assert!(stray.problem().is_none() && !stray.reserves_a_row());
}

#[test]
fn common_spellings_of_off_all_disable_the_row() {
    for spelling in [
        r#""off""#,
        r#""none""#,
        r#""hidden""#,
        r#""DISABLED""#,
        r#"" Off ""#,
    ] {
        let section = ui(&format!(r#"{{"type": {spelling}}}"#)).status_line;
        assert_eq!(
            section.declared_kind(),
            Some(StatusLineType::Disabled),
            "{spelling} should switch the row off"
        );
        assert!(
            section.problem().is_none() && !section.reserves_a_row(),
            "{spelling} is a clean disable, not a problem"
        );
    }

    // The same rule reaches the modes that are not `disabled`, which an alias
    // list matched on its own would leave parsing by a stricter one.
    let padded = ui(r#"{"type": " Builtin "}"#).status_line;
    assert_eq!(padded.declared_kind(), Some(StatusLineType::Builtin));

    // The items read by the same rule: a user who capitalises one gets the row
    // rather than a problem naming their own spelling back at them.
    let items = ui(r#"{"type": "builtin", "items": ["CWD", " model "]}"#).status_line;
    assert!(items.problem().is_none(), "{:?}", items.problem());
    assert_eq!(
        items.resolve(),
        Some(ResolvedStatusLine::Builtin {
            items: &[StatusLineItem::Cwd, StatusLineItem::Model]
        })
    );
    assert_eq!(
        StatusLineType::Disabled.as_str(),
        "disabled",
        "an alias must not become the name the config writes back"
    );
}

#[test]
fn row_with_content_to_draw_paints_no_problem_over_it() {
    let row = ui(r#"{"type": "command", "command": "x", "padding": "2"}"#).status_line;
    assert!(row.problem().is_some(), "the padding is still reported");
    assert!(row.problem_to_paint().is_none(), "the row draws its output");
}

#[test]
fn mode_without_its_payload_draws_the_problem_instead() {
    for orphan in [
        StatusLineConfigFixture::from_kind(StatusLineType::Command).into_config(),
        StatusLineConfigFixture::from_kind(StatusLineType::Command)
            .with_command("   ")
            .into_config(),
        StatusLineConfigFixture::from_kind(StatusLineType::Builtin)
            .with_items(Vec::new())
            .into_config(),
        ui(r#"{"command": "~/status_line.sh"}"#).status_line,
    ] {
        assert!(orphan.resolve().is_none(), "{orphan:?}");
        assert!(orphan.problem().is_some(), "{orphan:?}");
        assert!(orphan.reserves_a_row(), "a row to land in: {orphan:?}");
    }

    let ok = StatusLineConfigFixture::from_kind(StatusLineType::Command)
        .with_command("x")
        .into_config();
    assert!(ok.reserves_a_row() && ok.problem().is_none());

    let off = StatusLineConfig::default();
    assert!(!off.reserves_a_row() && off.problem().is_none());
}

#[test]
fn problem_does_not_make_a_default_config_look_touched() {
    assert!(ui(r#"{"type": "nope"}"#).status_line.is_default());
}

#[test]
fn each_problem_is_reported_once_however_the_bad_values_interleave() {
    let ui = ui(r#"{"type": "builtin", "padding": "x", "items": [7, "brnach", 8]}"#).status_line;
    assert_eq!(
        ui.problem(),
        Some("[ui.status_line] ignored items, items = \"brnach\", padding"),
        "one report per problem, in the order they were found"
    );
}

#[test]
fn every_item_round_trips_through_its_label() {
    for item in StatusLineItem::ALL {
        assert_eq!(StatusLineItem::parse(item.as_str()), Some(*item));
        assert_eq!(serde_json::to_value(item).unwrap(), json!(item.as_str()));
    }
    for kind in StatusLineType::VARIANTS {
        assert_eq!(StatusLineType::parse(kind.as_str()), Some(*kind));
        assert_eq!(serde_json::to_value(kind).unwrap(), json!(kind.as_str()));
    }
}

#[test]
fn every_field_survives_a_save_and_a_reload() {
    let saved = StatusLineConfig {
        kind: Some(StatusLineType::Builtin),
        command: Some("~/status_line.sh".into()),
        items: Some(vec![StatusLineItem::Cwd, StatusLineItem::TurnTimer]),
        padding: Some(2),
        refresh_interval: Some(300),
        parse_problem: Some("not written".into()),
        unknown_keys: vec!["colour".into()],
    };

    let written = serde_json::to_value(&saved).expect("the section serializes");
    assert_eq!(
        written,
        json!({
            "type": "builtin", "command": "~/status_line.sh",
            "items": ["cwd", "turn-timer"], "padding": 2, "refresh_interval": 300,
        }),
        "a problem and an unknown key are not settings to write back"
    );

    let reloaded: StatusLineConfig =
        serde_json::from_value(written).expect("what we wrote parses back");
    assert_eq!(reloaded, saved);
    assert!(reloaded.parse_problem.is_none() && reloaded.unknown_keys.is_empty());
}

#[test]
fn only_a_row_that_can_change_mid_turn_keeps_recomputing_through_one() {
    use StatusLineItem::{Cwd, TurnTimer};
    use StatusLineType::{Builtin, Command, Disabled};

    fn section(kind: StatusLineType, items: &[StatusLineItem]) -> StatusLineConfig {
        StatusLineConfigFixture::from_kind(kind)
            .with_items(items.to_vec())
            .into_config()
    }

    // Every segment against its own answer, so a row that stops asking one of
    // them fails here rather than freezing mid-turn.
    for item in StatusLineItem::ALL {
        let row = section(Builtin, &[*item]);
        assert_eq!(
            row.changes_during_a_turn(),
            item.varies_mid_turn(),
            "{row:?}"
        );
    }
    assert!(TurnTimer.varies_mid_turn(), "a timer counts on its own");
    assert!(!Cwd.varies_mid_turn(), "a directory does not move mid-turn");

    for (kind, items, changes) in [
        // A script may read a clock, so `command` always can.
        (Command, &[][..], true),
        // One segment that varies is enough for the row.
        (Builtin, &[Cwd, TurnTimer][..], true),
        (Builtin, &[Cwd][..], false),
        (Disabled, &[][..], false),
    ] {
        let row = section(kind, items);
        assert_eq!(row.changes_during_a_turn(), changes, "{row:?}");
    }
}

#[test]
fn unusable_numbers_are_capped_where_they_are_read() {
    let extreme = StatusLineConfigFixture::default()
        .with_padding(4000)
        .into_config();
    assert_eq!(extreme.padding(), StatusLineConfig::MAX_PADDING_PER_SIDE);

    let two = StatusLineConfigFixture::default()
        .with_padding(2)
        .into_config();
    assert_eq!(two.padding(), 2);
    assert_eq!(StatusLineConfig::default().padding(), 0);
}

#[test]
fn refresh_interval_is_command_only_and_clamped() {
    let floored = StatusLineConfigFixture::from_kind(StatusLineType::Command)
        .with_command("x")
        .with_refresh_interval(Some(0))
        .into_config();
    assert_eq!(
        floored.refresh_interval(),
        Some(Duration::from_secs(
            StatusLineConfig::MIN_REFRESH_INTERVAL_SECS
        )),
        "zero would re-run the script back to back"
    );

    let capped = StatusLineConfigFixture::from_kind(StatusLineType::Command)
        .with_command("x")
        .with_refresh_interval(Some(i64::MAX as u64))
        .into_config();
    assert_eq!(
        capped.refresh_interval(),
        Some(Duration::from_secs(
            StatusLineConfig::MAX_REFRESH_INTERVAL_SECS
        )),
        "unclamped, this value panics the event loop's `Instant::now() + interval`"
    );

    let unset = ui(r#"{"type": "command", "command": "x"}"#).status_line;
    assert_eq!(unset.refresh_interval(), None, "unset stays event-driven");

    // A command section that resolves nothing schedules nothing.
    let orphan = StatusLineConfigFixture::from_kind(StatusLineType::Command)
        .with_refresh_interval(Some(300))
        .into_config();
    assert_eq!(orphan.refresh_interval(), None);

    let builtin = ui(r#"{"type": "builtin", "refresh_interval": 300}"#).status_line;
    assert_eq!(builtin.refresh_interval(), None);
    assert!(
        builtin
            .problem()
            .is_some_and(|p| p.contains("refresh_interval needs type = \"command\"")),
        "a timer under builtin is reported rather than left looking like it refreshes"
    );
    assert!(
        builtin.problem_to_paint().is_none(),
        "the builtin row still draws its segments"
    );

    let empty = r#"{"type": "builtin", "items": [], "refresh_interval": 300}"#;
    names_the_problem(ui(empty), "needs at least one item", empty);

    // The off switch outranks its neighbours, like a stray `command` does.
    let off = ui(r#"{"type": "disabled", "refresh_interval": 300}"#).status_line;
    assert!(off.refresh_interval().is_none(), "off schedules nothing");
    assert!(off.problem().is_none() && !off.reserves_a_row());
}
