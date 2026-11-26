use std::path::Path;

use crate::implementations::skills::types::SkillInfo;

use super::SkillManager;

fn skill(name: &str, path: &str) -> SkillInfo {
    SkillInfo {
        name: name.to_owned(),
        path: path.to_owned(),
        ..SkillInfo::default()
    }
}

fn seeded_manager(skills: Vec<SkillInfo>) -> SkillManager {
    let mut manager = SkillManager::new();
    manager.seed(None, None, skills, None, None, None);
    manager
}

#[test]
fn suggests_unique_registered_path_for_wrong_root() {
    let manager = seeded_manager(vec![skill(
        "code-review",
        "/home/user/.echo-agent/skills/code-review/SKILL.md",
    )]);

    let suggestion = manager
        .suggest_skill_path(Path::new("/wrong/root/skills/code-review/SKILL.md"))
        .unwrap();

    assert_eq!(
        suggestion.display_path,
        Path::new("/home/user/.echo-agent/skills/code-review/SKILL.md")
    );
}

#[test]
fn suggests_nothing_for_ambiguous_disabled_non_skill_or_exact_requests() {
    let mut retired = skill("retired", "/home/user/.echo-agent/skills/retired/SKILL.md");
    retired.enabled = false;
    let manager = seeded_manager(vec![
        skill("review", "/repo/.echo-agent/skills/review/SKILL.md"),
        skill("review", "/home/user/.echo-agent/skills/review/SKILL.md"),
        retired,
        skill("solo", "/home/user/.echo-agent/skills/solo/SKILL.md"),
    ]);

    // Control: the manager does produce a suggestion for a clean miss.
    assert!(
        manager
            .suggest_skill_path(Path::new("/wrong/root/solo/SKILL.md"))
            .is_some()
    );

    for requested in [
        // Two registered candidates for the name.
        "/wrong/root/review/SKILL.md",
        // Disabled skill.
        "/wrong/root/retired/SKILL.md",
        // Not a SKILL.md read.
        "/wrong/root/solo/README.md",
        // The read already targeted the registered path.
        "/home/user/.echo-agent/skills/solo/SKILL.md",
    ] {
        assert!(
            manager.suggest_skill_path(Path::new(requested)).is_none(),
            "{requested}"
        );
    }
}

#[test]
fn requested_path_among_same_named_registrations_is_ambiguous() {
    // The failed read targets one registered path for "review"; another
    // same-named registration exists. Counting the requested path as a match
    // keeps this ambiguous (no suggestion) rather than treating the sibling
    // as a unique alternate.
    let manager = seeded_manager(vec![
        skill("review", "/repo/.echo-agent/skills/review/SKILL.md"),
        skill("review", "/home/user/.echo-agent/skills/review/SKILL.md"),
    ]);

    for requested in [
        "/repo/.echo-agent/skills/review/SKILL.md",
        "/home/user/.echo-agent/skills/review/SKILL.md",
    ] {
        assert!(
            manager.suggest_skill_path(Path::new(requested)).is_none(),
            "{requested}"
        );
    }
}

#[test]
fn includes_model_disabled_and_held_conditional_skills() {
    let mut model_disabled = skill("manual", "/home/user/.echo-agent/skills/manual/SKILL.md");
    model_disabled.disable_model_invocation = true;
    // `paths:`-gated skills are withheld from the listing but still registered,
    // whether seeded or dynamically discovered.
    let mut gated = skill("gated", "/repo/.echo-agent/skills/gated/SKILL.md");
    gated.paths = Some(vec!["src/**".to_owned()]);
    let mut manager = seeded_manager(vec![model_disabled, gated]);

    let mut dynamic = skill(
        "conditional",
        "/home/user/.echo-agent/skills/conditional/SKILL.md",
    );
    dynamic.paths = Some(vec!["src/**".to_owned()]);
    assert!(!manager.add_discovered(vec![dynamic]));

    for requested in [
        "/wrong/root/manual/SKILL.md",
        "/wrong/root/gated/SKILL.md",
        "/wrong/root/conditional/SKILL.md",
    ] {
        assert!(
            manager.suggest_skill_path(Path::new(requested)).is_some(),
            "{requested}"
        );
    }
}

#[test]
fn baseline_reload_updates_lookup_but_not_snapshot_names() {
    let mut manager = SkillManager::new();
    manager.set_discovery_snapshot_names(vec!["initial".to_owned()]);
    manager.seed(
        None,
        None,
        vec![skill(
            "initial",
            "/repo/.echo-agent/skills/initial/SKILL.md",
        )],
        None,
        None,
        None,
    );

    manager.update_startup_baseline(vec![skill(
        "reloaded",
        "/repo/.echo-agent/skills/reloaded/SKILL.md",
    )]);

    // Session-start names are immutable across reloads, while lookup follows
    // the current baseline: a removed skill is no longer suggested.
    assert_eq!(
        manager.discovery_snapshot_names(),
        vec!["initial".to_owned()]
    );
    assert!(
        manager
            .suggest_skill_path(Path::new("/wrong/root/reloaded/SKILL.md"))
            .is_some()
    );
    assert!(
        manager
            .suggest_skill_path(Path::new("/wrong/root/initial/SKILL.md"))
            .is_none()
    );
}

#[test]
fn reload_disabling_a_discovered_skill_stops_suggesting_it() {
    let path = "/repo/.echo-agent/skills/review/SKILL.md";
    let mut manager = seeded_manager(Vec::new());
    manager.add_discovered(vec![skill("review", path)]);

    let mut now_disabled = skill("review", path);
    now_disabled.enabled = false;
    manager.update_startup_baseline(vec![now_disabled]);

    // The reloaded baseline record owns the canonical path even though it is
    // disabled, so the older enabled dynamic record at the same path can
    // neither be suggested nor count as a second match.
    assert!(
        manager
            .suggest_skill_path(Path::new("/wrong/root/review/SKILL.md"))
            .is_none()
    );
}

#[test]
fn reload_moving_a_skill_suggests_only_the_current_registration() {
    // Baseline-only move: the old file may still exist on disk, but only the
    // current registration counts, so the moved path is unique.
    let mut manager = seeded_manager(vec![skill(
        "review",
        "/repo/old/.echo-agent/skills/review/SKILL.md",
    )]);
    manager.update_startup_baseline(vec![skill(
        "review",
        "/repo/new/.echo-agent/skills/review/SKILL.md",
    )]);
    let suggestion = manager
        .suggest_skill_path(Path::new("/wrong/root/review/SKILL.md"))
        .unwrap();
    assert_eq!(
        suggestion.display_path,
        Path::new("/repo/new/.echo-agent/skills/review/SKILL.md")
    );

    // With a stale dynamic record left at the old path, lookup cannot tell a
    // stale record from a genuinely distinct same-name skill, so it fails safe
    // with no suggestion rather than risk pointing at the wrong SKILL.md.
    let mut manager = seeded_manager(Vec::new());
    manager.add_discovered(vec![skill(
        "review",
        "/repo/old/.echo-agent/skills/review/SKILL.md",
    )]);
    manager.update_startup_baseline(vec![skill(
        "review",
        "/repo/new/.echo-agent/skills/review/SKILL.md",
    )]);
    assert!(
        manager
            .suggest_skill_path(Path::new("/wrong/root/review/SKILL.md"))
            .is_none()
    );
}

#[test]
fn deduplicates_discovered_and_baseline_copies() {
    let path = "/repo/.echo-agent/skills/review/SKILL.md";
    let mut manager = seeded_manager(vec![skill("review", path)]);
    manager.add_discovered(vec![skill("review", path)]);

    let suggestion = manager
        .suggest_skill_path(Path::new("/wrong/root/review/SKILL.md"))
        .unwrap();

    assert_eq!(suggestion.display_path, Path::new(path));
}

#[test]
fn rewrites_worktree_paths_to_display_cwd_but_preserves_external_paths() {
    let mut manager = seeded_manager(vec![
        skill(
            "review",
            "/real/worktree/.echo-agent/skills/review/SKILL.md",
        ),
        skill(
            "external",
            "/home/user/.echo-agent/skills/external/SKILL.md",
        ),
    ]);
    manager.real_cwd_prefix = Some("/real/worktree".to_owned());
    manager.display_cwd = Some("/display/project".to_owned());

    assert_eq!(
        manager
            .suggest_skill_path(Path::new("/wrong/root/review/SKILL.md"))
            .unwrap()
            .display_path,
        Path::new("/display/project/.echo-agent/skills/review/SKILL.md")
    );
    assert_eq!(
        manager
            .suggest_skill_path(Path::new("/wrong/root/external/SKILL.md"))
            .unwrap()
            .display_path,
        Path::new("/home/user/.echo-agent/skills/external/SKILL.md")
    );
}
