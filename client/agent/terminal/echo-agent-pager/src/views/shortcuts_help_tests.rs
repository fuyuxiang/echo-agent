use super::*;
use crate::key;

struct VimModeGuard(bool);

impl VimModeGuard {
    fn set(enabled: bool) -> Self {
        let previous = crate::appearance::cache::load_vim_mode();
        crate::appearance::cache::set_vim_mode(enabled);
        Self(previous)
    }
}

impl Drop for VimModeGuard {
    fn drop(&mut self) {
        crate::appearance::cache::set_vim_mode(self.0);
    }
}

fn header(label: &'static str, idx: usize, count: usize) -> ShortcutsHelpEntry {
    ShortcutsHelpEntry::SectionHeader {
        label,
        category_idx: idx,
        entry_count: count,
    }
}

fn hint(label: &'static str, k: KeyShortcut) -> ShortcutsHelpEntry {
    ShortcutsHelpEntry::Hint {
        item: HintItem::new(k, label),
        dimmed: false,
        action_id: None,
        long_help: None,
    }
}

fn no_collapsed() -> std::collections::HashSet<usize> {
    std::collections::HashSet::new()
}

fn no_expanded() -> std::collections::HashSet<ExpandKey> {
    std::collections::HashSet::new()
}

fn browse_mode() -> ShortcutsHelpMode {
    ShortcutsHelpMode::Browse
}

fn hint_with_action(
    label: &'static str,
    k: KeyShortcut,
    action_id: crate::actions::ActionId,
) -> ShortcutsHelpEntry {
    let mut item = HintItem::new(k, label);
    item.description = Some(std::borrow::Cow::Borrowed(label));
    ShortcutsHelpEntry::Hint {
        item,
        dimmed: false,
        action_id: Some(action_id),
        long_help: None,
    }
}

/// `DashboardCycleMode` carries Shift+Tab three times (the terminal
/// encoding variants `BackTab` / `BackTab`+SHIFT / `Tab`+SHIFT).
/// The cheatsheet must collapse identically-rendered keys instead
/// of showing "Shift+Tab / Shift+Tab / Shift+Tab".
#[test]
fn build_entries_dedupes_identically_rendered_alt_keys() {
    let registry = crate::actions::ActionRegistry::defaults();
    let entries = build_entries(&[When::DashboardFocused], &registry, false);
    let item = entries
        .iter()
        .find_map(|e| match e {
            ShortcutsHelpEntry::Hint { item, .. }
                if item.description.as_deref() == Some("Cycle dispatch mode") =>
            {
                Some(item)
            }
            _ => None,
        })
        .expect("DashboardCycleMode must be listed");
    assert_eq!(
        hint_key_pretty(item),
        "Shift+Tab",
        "encoding-variant alt keys must collapse to one display",
    );
}

#[test]
fn build_entries_lists_prompt_stash_with_ctrl_s_and_alt_s() {
    let registry = crate::actions::ActionRegistry::defaults();
    let entries = build_entries(&[When::PromptFocused], &registry, false);
    let alt = if cfg!(target_os = "macos") {
        "Opt"
    } else {
        "Alt"
    };

    let (item, dimmed) = entries
        .iter()
        .find_map(|e| match e {
            ShortcutsHelpEntry::Hint {
                item,
                dimmed,
                action_id: Some(crate::actions::ActionId::StashPrompt),
                ..
            } => Some((item, *dimmed)),
            _ => None,
        })
        .expect("StashPrompt must be listed in the shortcuts window");

    assert!(!dimmed, "stash must be lit while the prompt is focused");
    assert_eq!(hint_key_pretty(item), format!("Ctrl+s / {alt}+s"));
}

#[test]
fn filter_empty_query_returns_all_indices() {
    let entries = vec![
        header("Nav", 0, 2),
        hint("send", key!(Enter)),
        hint("nav", key!('j')),
        header("App", 1, 1),
        hint("quit", key!('q', CONTROL)),
    ];
    assert_eq!(
        filter_entries(&entries, "", false, &no_collapsed()),
        vec![0, 1, 2, 3, 4]
    );
}

#[test]
fn filter_keeps_header_when_section_has_match() {
    let entries = vec![
        header("Nav", 0, 2),
        hint("send", key!(Enter)),
        hint("nav", key!('j')),
        header("App", 1, 1),
        hint("quit", key!('q', CONTROL)),
    ];
    assert_eq!(
        filter_entries(&entries, "send", false, &no_collapsed()),
        vec![0, 1]
    );
}

#[test]
fn filter_drops_header_when_section_empty() {
    let entries = vec![
        header("Nav", 0, 1),
        hint("send", key!(Enter)),
        header("App", 1, 1),
        hint("quit", key!('q', CONTROL)),
    ];
    assert_eq!(
        filter_entries(&entries, "quit", false, &no_collapsed()),
        vec![2, 3]
    );
}

#[test]
fn filter_matches_against_key_display() {
    let entries = vec![
        header("Nav", 0, 2),
        hint("send", key!(Enter)),
        hint("nav", key!('j')),
    ];
    assert_eq!(
        filter_entries(&entries, "enter", false, &no_collapsed()),
        vec![0, 1]
    );
}

#[test]
fn filter_keeps_both_headers_when_both_sections_match() {
    let entries = vec![
        header("Nav", 0, 1),
        hint("nav", key!('j')),
        header("App", 1, 1),
        hint("new session", key!('n', CONTROL)),
    ];
    let result = filter_entries(&entries, "n", false, &no_collapsed());
    assert!(result.contains(&0));
    assert!(result.contains(&1));
    assert!(result.contains(&2));
    assert!(result.contains(&3));
}

#[test]
fn collapsed_section_shows_header_only() {
    let entries = vec![
        header("Nav", 0, 2),
        hint("send", key!(Enter)),
        hint("nav", key!('j')),
        header("App", 1, 1),
        hint("quit", key!('q', CONTROL)),
    ];
    let mut collapsed = std::collections::HashSet::new();
    collapsed.insert(0);
    let result = filter_entries(&entries, "", false, &collapsed);
    assert_eq!(result, vec![0, 3, 4]);
}

#[test]
fn search_forces_collapsed_sections_open() {
    let entries = vec![
        header("Nav", 0, 1),
        hint("nav", key!('j')),
        header("App", 1, 1),
        hint("quit", key!('q', CONTROL)),
    ];
    let mut collapsed = std::collections::HashSet::new();
    collapsed.insert(0);
    let result = filter_entries(&entries, "nav", false, &collapsed);
    assert!(
        result.contains(&1),
        "search should find nav in collapsed section"
    );
}

fn all_contexts() -> Vec<When> {
    vec![
        When::ScrollbackFocused,
        When::PromptFocused,
        When::AgentScreen,
        When::Always,
    ]
}

#[test]
fn build_entries_groups_by_category() {
    let registry = ActionRegistry::defaults();
    let entries = build_entries(&all_contexts(), &registry, true);

    let headers: Vec<&str> = entries
        .iter()
        .filter_map(|e| match e {
            ShortcutsHelpEntry::SectionHeader { label, .. } => Some(*label),
            _ => None,
        })
        .collect();
    assert!(headers.contains(&"Essentials"));
    assert!(headers.contains(&"Conversation Navigation"));
    assert!(headers.contains(&"Panels"));
}

#[test]
fn mouse_reporting_shortcut_absent_by_default() {
    // Opt-in via config.toml; default registry must not advertise it.
    let registry = ActionRegistry::defaults();
    assert!(registry.find(ActionId::ToggleMouseCapture).is_none());
    let entries = build_entries(&all_contexts(), &registry, true);
    let has_row = entries.iter().any(|e| {
        matches!(
            e,
            ShortcutsHelpEntry::Hint { item, .. } if item.label == "mouse reporting"
        )
    });
    assert!(
        !has_row,
        "mouse reporting must not appear when config-disabled"
    );
}

#[test]
fn mouse_reporting_shortcut_is_under_panels_when_enabled() {
    let registry = ActionRegistry::defaults_with_config(true);
    let def = registry
        .find(ActionId::ToggleMouseCapture)
        .expect("ToggleMouseCapture action must be registered when config-enabled");
    assert_eq!(def.category, Category::Panels);
    assert_eq!(def.label, "mouse reporting");
    assert_eq!(
        def.description,
        "Toggle mouse reporting (native copy/paste)",
    );

    let entries = build_entries(&all_contexts(), &registry, true);
    let mut in_panels = false;
    let mut in_essentials = false;
    let mut seen = false;
    for entry in &entries {
        match entry {
            ShortcutsHelpEntry::SectionHeader { label, .. } => {
                in_panels = *label == "Panels";
                in_essentials = *label == "Essentials";
            }
            ShortcutsHelpEntry::Hint { item, .. } => {
                if item.label == "mouse reporting" {
                    assert!(
                        in_panels,
                        "mouse reporting row must be in Panels, not Essentials"
                    );
                    assert!(
                        !in_essentials,
                        "mouse reporting must not appear under Essentials"
                    );
                    assert_eq!(
                        item.description.as_deref(),
                        Some("Toggle mouse reporting (native copy/paste)"),
                    );
                    let key_text = hint_key_pretty(item);
                    assert!(
                        key_text.contains("Ctrl+r") || key_text.contains("Ctrl+R"),
                        "expected Ctrl+r in key display, got {key_text:?}"
                    );
                    seen = true;
                }
            }
        }
    }
    assert!(
        seen,
        "mouse reporting row must be present in shortcuts help when enabled"
    );
}

#[test]
fn build_entries_deduplicates_within_category() {
    let registry = ActionRegistry::defaults();
    let entries = build_entries(&all_contexts(), &registry, true);

    let mut current_cat_keys: std::collections::HashSet<KeyShortcut> =
        std::collections::HashSet::new();
    for entry in &entries {
        match entry {
            ShortcutsHelpEntry::SectionHeader { .. } => {
                current_cat_keys.clear();
            }
            ShortcutsHelpEntry::Hint { item: h, .. } => {
                if let Some(&k) = h.keys.first() {
                    assert!(
                        current_cat_keys.insert(k),
                        "duplicate key {:?} within same category",
                        k.display()
                    );
                }
            }
        }
    }
}

#[test]
fn build_entries_show_mode_correct_ctrl_g_and_shared_ctrl_b() {
    for mode in [
        crate::app::ScreenMode::Fullscreen,
        crate::app::ScreenMode::Inline,
        crate::app::ScreenMode::Minimal,
    ] {
        let registry = ActionRegistry::defaults_for(mode);
        let prompt_contexts = [When::PromptFocused, When::AgentScreen, When::Always];
        let entries = build_entries(&prompt_contexts, &registry, true);

        let row = |action: ActionId| {
            entries.iter().find_map(|entry| match entry {
                ShortcutsHelpEntry::Hint {
                    item,
                    action_id: Some(id),
                    ..
                } if *id == action => Some(item),
                _ => None,
            })
        };
        let background = row(ActionId::SendToBackground).expect("background row");
        assert_eq!(background.keys, vec![crate::key!('b', CONTROL)]);

        let agent_ctrl_g_rows: Vec<_> = entries
            .iter()
            .filter_map(|entry| match entry {
                ShortcutsHelpEntry::Hint {
                    item,
                    action_id: Some(id),
                    ..
                } if item.keys.contains(&crate::key!('g', CONTROL))
                    && registry
                        .find(*id)
                        .is_some_and(|def| def.context == When::AgentScreen) =>
                {
                    Some(*id)
                }
                _ => None,
            })
            .collect();
        if mode.is_minimal() {
            assert!(row(ActionId::FocusScrollback).is_none());
        } else {
            assert!(row(ActionId::FocusScrollback).is_some());
        }

        let expected = if mode.is_minimal() {
            ActionId::EditPromptExternal
        } else {
            ActionId::ToggleTasks
        };
        assert_eq!(agent_ctrl_g_rows, vec![expected]);
        assert!(row(expected).is_some());
        assert!(
            row(if mode.is_minimal() {
                ActionId::ToggleTasks
            } else {
                ActionId::EditPromptExternal
            })
            .is_none()
        );
    }
}

#[test]
fn build_entries_includes_new_pane_actions() {
    let registry = ActionRegistry::defaults();
    let entries = build_entries(&all_contexts(), &registry, true);

    let has_todos = entries.iter().any(|e| {
        matches!(
            e,
            ShortcutsHelpEntry::Hint { item, .. } if item.label == "todos"
        )
    });
    let has_sessions = entries.iter().any(|e| {
        matches!(
            e,
            ShortcutsHelpEntry::Hint { item, .. } if item.label == "sessions"
        )
    });
    let has_queue = entries.iter().any(|e| {
        matches!(
            e,
            ShortcutsHelpEntry::Hint { item, .. } if item.label == "queue"
        )
    });
    assert!(has_todos, "should include toggle todos");
    assert!(has_sessions, "should include open sessions");
    assert!(has_queue, "should include toggle queue");
}

fn has_scrollback_search(entries: &[ShortcutsHelpEntry]) -> bool {
    entries.iter().any(|e| {
        matches!(
            e,
            ShortcutsHelpEntry::Hint { item, .. }
                if item.label == "search" && item.keys.iter().any(|k| k.display() == "/")
        )
    })
}

fn has_find_search(entries: &[ShortcutsHelpEntry]) -> bool {
    entries.iter().any(|e| {
        matches!(
            e,
            ShortcutsHelpEntry::Hint { item, .. }
                if item.custom_display == Some("/find")
        )
    })
}

fn history_row(entries: &[ShortcutsHelpEntry]) -> Option<&ShortcutsHelpEntry> {
    entries.iter().find(|e| {
        matches!(
            e,
            ShortcutsHelpEntry::Hint { item, action_id: None, .. }
                if item.label == "history"
        )
    })
}

#[test]
fn build_entries_includes_scrollback_search_in_vim_mode() {
    let registry = ActionRegistry::defaults();
    let entries = build_entries(&all_contexts(), &registry, true);
    assert!(
        has_scrollback_search(&entries),
        "vim cheatsheet should list / search"
    );
    assert!(
        !has_find_search(&entries),
        "vim mode uses the `/` key row, not the /find slash row"
    );
}

#[test]
fn build_entries_includes_find_search_in_simple_mode() {
    let registry = ActionRegistry::defaults();
    let entries = build_entries(&all_contexts(), &registry, false);
    assert!(
        has_find_search(&entries),
        "simple mode should list the /find scrollback search"
    );
    assert!(
        !has_scrollback_search(&entries),
        "simple mode must not list the bare `/` key row"
    );
}

#[test]
fn build_entries_includes_history_row_in_both_modes() {
    let registry = ActionRegistry::defaults();
    for vim in [true, false] {
        let entries = build_entries(&all_contexts(), &registry, vim);
        assert!(
            history_row(&entries).is_some(),
            "history row should appear in vim={vim} mode"
        );
    }
}

#[test]
fn history_row_lit_only_by_prompt_focus() {
    let registry = ActionRegistry::defaults();

    let entries = build_entries(&[When::PromptFocused], &registry, false);
    let ShortcutsHelpEntry::Hint { dimmed, .. } =
        history_row(&entries).expect("history row present")
    else {
        unreachable!();
    };
    assert!(
        !*dimmed,
        "history row must be lit when the prompt is focused"
    );

    let entries = build_entries(&[When::ScrollbackFocused], &registry, false);
    let ShortcutsHelpEntry::Hint { dimmed, .. } =
        history_row(&entries).expect("history row present")
    else {
        unreachable!();
    };
    assert!(*dimmed, "history row must be dimmed without prompt focus");

    // Dashboard focus alone must not light it (unlike paste/undo/redo).
    let entries = build_entries(&[When::DashboardFocused], &registry, false);
    let ShortcutsHelpEntry::Hint { dimmed, .. } =
        history_row(&entries).expect("history row present")
    else {
        unreachable!();
    };
    assert!(
        *dimmed,
        "dashboard focus alone must not light the history row"
    );
}

#[test]
fn build_entries_includes_paste() {
    let registry = ActionRegistry::defaults();
    let entries = build_entries(&all_contexts(), &registry, true);
    let entry = entries
        .iter()
        .find(|e| {
            matches!(
                e,
                ShortcutsHelpEntry::Hint {
                    item,
                    action_id: None,
                    ..
                } if item.label == "paste"
            )
        })
        .expect("cheatsheet should list paste");
    let ShortcutsHelpEntry::Hint {
        item, long_help, ..
    } = entry
    else {
        unreachable!();
    };
    assert!(item.keys.iter().any(|k| *k == key!('v', CONTROL)));
    assert!(
        item.description
            .as_deref()
            .is_some_and(|d| d.to_lowercase().contains("image")),
        "description should mention image for search"
    );
    assert_eq!(*long_help, Some(PASTE_LONG_HELP));
    assert!(
        PASTE_LONG_HELP.contains('\n'),
        "paste long_help should be multi-line man-style"
    );
    #[cfg(target_os = "windows")]
    assert!(item.keys.iter().any(|k| *k == key!('v', ALT)));
    #[cfg(not(target_os = "windows"))]
    assert!(!item.keys.iter().any(|k| *k == key!('v', ALT)));
}

/// Display-only Input rows for textarea undo/redo (mirrors paste).
#[test]
fn build_entries_lists_undo_and_redo() {
    let registry = ActionRegistry::defaults();
    let entries = build_entries(&all_contexts(), &registry, true);

    let (undo_keys, undo_help) = pseudo_hint(&entries, "undo").expect("undo row");
    assert!(undo_keys.contains(&key!('z', CONTROL)));
    assert_eq!(undo_help, Some(UNDO_LONG_HELP));

    let (redo_keys, redo_help) = pseudo_hint(&entries, "redo").expect("redo row");
    assert!(redo_keys.contains(&key!('z', CONTROL | SHIFT)));
    assert!(redo_keys.contains(&key!('r', CONTROL)));
    assert_eq!(redo_help, Some(REDO_LONG_HELP));
}

fn pseudo_hint<'a>(
    entries: &'a [ShortcutsHelpEntry],
    label: &str,
) -> Option<(&'a [KeyShortcut], Option<&'static str>)> {
    entries.iter().find_map(|e| match e {
        ShortcutsHelpEntry::Hint {
            item,
            action_id: None,
            long_help,
            ..
        } if item.label == label => Some((item.keys.as_slice(), *long_help)),
        _ => None,
    })
}

fn pseudo_dimmed(entries: &[ShortcutsHelpEntry], label: &str) -> Option<bool> {
    entries.iter().find_map(|e| match e {
        ShortcutsHelpEntry::Hint {
            item,
            dimmed,
            action_id: None,
            ..
        } if item.label == label => Some(*dimmed),
        _ => None,
    })
}

#[test]
fn build_entries_dims_editor_pseudo_rows_outside_prompt_and_dashboard() {
    let registry = ActionRegistry::defaults();
    // paste / undo / redo share the same host lit/dim policy.
    for label in ["paste", "undo", "redo"] {
        assert_eq!(
            pseudo_dimmed(
                &build_entries(
                    &[When::ScrollbackFocused, When::AgentScreen, When::Always],
                    &registry,
                    true,
                ),
                label,
            ),
            Some(true),
            "{label} dimmed off prompt/dashboard"
        );
        assert_eq!(
            pseudo_dimmed(
                &build_entries(
                    &[When::PromptFocused, When::AgentScreen, When::Always],
                    &registry,
                    true,
                ),
                label,
            ),
            Some(false),
            "{label} lit when prompt focused"
        );
        assert_eq!(
            pseudo_dimmed(
                &build_entries(&[When::DashboardFocused, When::Always], &registry, true),
                label,
            ),
            Some(false),
            "{label} lit on dashboard host"
        );
    }
}

#[test]
fn build_entries_dims_out_of_context_actions() {
    let registry = ActionRegistry::defaults();
    let prompt_contexts = vec![When::PromptFocused, When::AgentScreen, When::Always];
    let entries = build_entries(&prompt_contexts, &registry, true);

    let nav_dimmed = entries.iter().any(|e| {
        matches!(
            e,
            ShortcutsHelpEntry::Hint { item, dimmed: true, .. } if item.label == "nav"
        )
    });
    assert!(
        nav_dimmed,
        "scrollback nav should be dimmed when prompt is focused"
    );

    let quit_bright = entries.iter().any(|e| {
        matches!(
            e,
            ShortcutsHelpEntry::Hint { item, dimmed: false, .. } if item.label == "quit"
        )
    });
    assert!(quit_bright, "quit should not be dimmed (When::Always)");

    let cancel_bright = entries.iter().any(|e| {
        matches!(
            e,
            ShortcutsHelpEntry::Hint { item, dimmed: false, .. } if item.label == "cancel"
        )
    });
    assert!(
        cancel_bright,
        "cancel should not be dimmed (When::AgentScreen)"
    );
}

#[test]
fn build_entries_dims_both_pane_contexts_from_side_pane() {
    let registry = ActionRegistry::defaults();
    let todo_contexts = vec![When::AgentScreen, When::Always];
    let entries = build_entries(&todo_contexts, &registry, true);

    let send_dimmed = entries.iter().any(|e| {
        matches!(
            e,
            ShortcutsHelpEntry::Hint { item, dimmed: true, .. } if item.label == "send"
        )
    });
    assert!(
        send_dimmed,
        "send should be dimmed from todo pane (PromptFocused)"
    );

    let nav_dimmed = entries.iter().any(|e| {
        matches!(
            e,
            ShortcutsHelpEntry::Hint { item, dimmed: true, .. } if item.label == "nav"
        )
    });
    assert!(
        nav_dimmed,
        "nav should be dimmed from todo pane (ScrollbackFocused)"
    );
}

/// The dashboard LIST and the session OVERLAY dim each other's shortcuts:
/// on the list the overlay-scoped shortcuts (`When::DashboardOverlay`,
/// e.g. "prev session") are dimmed while the list shortcuts
/// (`When::DashboardFocused`, e.g. "pin") are lit; inside the overlay it's
/// the inverse. (Dashboard actions are registered under `cfg(test)`.)
#[test]
fn build_entries_dims_dashboard_list_vs_overlay() {
    let registry = ActionRegistry::defaults();
    let dimmed_of = |entries: &[ShortcutsHelpEntry], label: &str| -> Option<bool> {
        entries.iter().find_map(|e| match e {
            ShortcutsHelpEntry::Hint { item, dimmed, .. } if item.label == label => Some(*dimmed),
            _ => None,
        })
    };

    // Dashboard LIST: list shortcuts lit, overlay shortcuts dimmed.
    let list = build_entries(&[When::DashboardFocused, When::Always], &registry, true);
    assert_eq!(
        dimmed_of(&list, "pin"),
        Some(false),
        "list `pin` must be lit on the dashboard list",
    );
    assert_eq!(
        dimmed_of(&list, "prev session"),
        Some(true),
        "overlay `prev session` must be dimmed on the dashboard list",
    );

    // Session OVERLAY (details): overlay shortcuts lit, list shortcuts dimmed.
    let overlay = build_entries(
        &[When::AgentScreen, When::Always, When::DashboardOverlay],
        &registry,
        true,
    );
    assert_eq!(
        dimmed_of(&overlay, "prev session"),
        Some(false),
        "overlay `prev session` must be lit inside the overlay",
    );
    assert_eq!(
        dimmed_of(&overlay, "pin"),
        Some(true),
        "list `pin` must be dimmed inside the overlay",
    );
}

/// `DashboardStop` (list) and `DashboardOverlayStop` (overlay) share
/// Ctrl+X and the Dashboard category. The per-category dedup must keep
/// whichever matches the active surface — lit — instead of always
/// keeping the first-registered (list) def. And inside the overlay the
/// `ShortcutsHelp` row must drop its shadowed Ctrl+X alt (the overlay
/// stop owns the key there) while keeping its other binding.
#[test]
fn build_entries_overlay_stop_wins_dedup_and_shadows_cheatsheet_ctrl_x() {
    let registry = ActionRegistry::defaults();
    let ctrl_x = crate::key!('x', CONTROL);
    // Match the two Ctrl+X rows by ActionId: the list and overlay
    // stops carry different labels ("delete" vs "stop").
    let is_stop = |action_id: &Option<ActionId>| {
        matches!(
            action_id,
            Some(ActionId::DashboardStop | ActionId::DashboardOverlayStop)
        )
    };
    let stop_rows = |entries: &[ShortcutsHelpEntry]| -> Vec<(String, bool)> {
        entries
            .iter()
            .filter_map(|e| match e {
                ShortcutsHelpEntry::Hint {
                    item,
                    dimmed,
                    action_id,
                    ..
                } if is_stop(action_id) => Some((
                    item.description.as_deref().unwrap_or_default().to_string(),
                    *dimmed,
                )),
                _ => None,
            })
            .collect()
    };
    let stop_id = |entries: &[ShortcutsHelpEntry]| -> Option<ActionId> {
        entries
            .iter()
            .find_map(|e| match e {
                ShortcutsHelpEntry::Hint { action_id, .. } if is_stop(action_id) => {
                    Some(*action_id)
                }
                _ => None,
            })
            .flatten()
    };
    let help_keys = |entries: &[ShortcutsHelpEntry]| -> Vec<KeyShortcut> {
        entries
            .iter()
            .find_map(|e| match e {
                ShortcutsHelpEntry::Hint { item, .. } if item.label == "shortcuts" => {
                    Some(item.keys.clone())
                }
                _ => None,
            })
            .expect("the ShortcutsHelp row must be present")
    };

    // Dashboard LIST: the list stop survives, lit; the cheatsheet
    // row keeps Ctrl+X (no overlay up).
    let list = build_entries(&[When::DashboardFocused, When::Always], &registry, true);
    assert_eq!(
        stop_rows(&list),
        vec![("Stop / Delete agent".to_string(), false)],
    );
    assert_eq!(
        stop_id(&list),
        Some(ActionId::DashboardStop),
        "the lit list `stop` is inserted first and never replaced — keeps DashboardStop",
    );
    assert!(
        help_keys(&list).contains(&ctrl_x),
        "without an overlay the cheatsheet row keeps its Ctrl+X binding",
    );

    // Session OVERLAY: the overlay stop survives, lit; the
    // cheatsheet row drops the shadowed Ctrl+X but keeps Ctrl+.
    let overlay = build_entries(
        &[When::AgentScreen, When::Always, When::DashboardOverlay],
        &registry,
        true,
    );
    assert_eq!(
        stop_rows(&overlay),
        vec![(
            "Stop agent, close session (back to dashboard)".to_string(),
            false
        )],
        "the overlay must show exactly the overlay `stop`, lit",
    );
    assert_eq!(
        stop_id(&overlay),
        Some(ActionId::DashboardOverlayStop),
        "the lit overlay `stop` replaces the dimmed list row — carries DashboardOverlayStop",
    );
    let keys = help_keys(&overlay);
    assert!(
        !keys.contains(&ctrl_x),
        "inside the overlay the cheatsheet row must drop the shadowed Ctrl+X",
    );
    assert!(
        !keys.is_empty(),
        "the cheatsheet row must keep its non-shadowed binding (Ctrl+.)",
    );
}

#[test]
fn initial_state_selects_first_hint_not_header() {
    let entries = vec![
        header("Nav", 0, 2),
        hint("send", key!(Enter)),
        hint("nav", key!('j')),
    ];
    let state = build_initial_picker_state(&entries);
    assert_eq!(state.selected, 1, "selected should land on first Hint");
}

// ── handle_input tests ───────────────────────────────────────

fn make_key(code: crossterm::event::KeyCode) -> crossterm::event::KeyEvent {
    crossterm::event::KeyEvent::new(code, crossterm::event::KeyModifiers::NONE)
}

/// Helper: set up entries + state with selected on a section header.
fn setup_on_header() -> (Vec<ShortcutsHelpEntry>, PickerState) {
    let entries = vec![
        header("Nav", 0, 2),
        hint("send", key!(Enter)),
        hint("nav", key!('j')),
    ];
    let mut state = build_initial_picker_state(&entries);
    state.selected = 0; // select the header
    (entries, state)
}

#[test]
fn space_on_section_header_toggles() {
    let (entries, mut state) = setup_on_header();
    let mut mode = browse_mode();
    let result = handle_input(
        &make_key(crossterm::event::KeyCode::Char(' ')),
        &entries,
        &mut state,
        false,
        &no_collapsed(),
        &no_expanded(),
        &mut mode,
    );
    assert_eq!(result, ShortcutsHelpOutcome::ToggleSection(0));
}

#[test]
fn enter_on_section_header_toggles() {
    let (entries, mut state) = setup_on_header();
    let mut mode = browse_mode();
    let result = handle_input(
        &make_key(crossterm::event::KeyCode::Enter),
        &entries,
        &mut state,
        false,
        &no_collapsed(),
        &no_expanded(),
        &mut mode,
    );
    assert_eq!(result, ShortcutsHelpOutcome::ToggleSection(0));
}

#[test]
fn enter_on_hint_without_action_id_is_unchanged() {
    // Pseudo/legacy hints have no action_id — Enter does not close or open detail.
    let entries = vec![header("Nav", 0, 1), hint("send", key!(Enter))];
    let mut state = build_initial_picker_state(&entries);
    state.selected = 1; // select the hint
    let mut mode = browse_mode();
    let result = handle_input(
        &make_key(crossterm::event::KeyCode::Enter),
        &entries,
        &mut state,
        false,
        &no_collapsed(),
        &no_expanded(),
        &mut mode,
    );
    assert_eq!(result, ShortcutsHelpOutcome::Unchanged);
    assert!(mode.is_browse());
}

#[test]
fn enter_on_registry_hint_opens_detail() {
    use crate::actions::ActionId;
    let entries = vec![
        header("Nav", 0, 1),
        hint_with_action("send", key!(Enter), ActionId::SendPrompt),
    ];
    let mut state = build_initial_picker_state(&entries);
    state.selected = 1;
    let mut mode = browse_mode();
    let result = handle_input(
        &make_key(crossterm::event::KeyCode::Enter),
        &entries,
        &mut state,
        false,
        &no_collapsed(),
        &no_expanded(),
        &mut mode,
    );
    assert_eq!(result, ShortcutsHelpOutcome::Changed);
    assert!(
        mode.is_detail(),
        "Enter on a registry hint must switch mode to Detail"
    );
}

/// Opening detail from an active search clears the query so a later Esc closes
/// the modal directly (back -> close), not back -> clear-query -> close.
#[test]
fn enter_from_search_opens_detail_and_clears_query() {
    use crate::actions::ActionId;
    let entries = vec![
        header("Nav", 0, 1),
        hint_with_action("send", key!(Enter), ActionId::SendPrompt),
    ];
    let mut state = build_initial_picker_state(&entries);
    // Active search matching the hint, selection on the matching row.
    state.set_query("send");
    state.search_active = true;
    state.selected = 1;
    let mut mode = browse_mode();
    let result = handle_input(
        &make_key(crossterm::event::KeyCode::Enter),
        &entries,
        &mut state,
        false,
        &no_collapsed(),
        &no_expanded(),
        &mut mode,
    );
    assert_eq!(result, ShortcutsHelpOutcome::Changed);
    assert!(mode.is_detail(), "Enter from search opens the detail page");
    assert!(
        state.query().is_empty(),
        "opening detail clears the search query"
    );
    assert!(!state.search_active, "opening detail clears search_active");
}

/// Mouse parity with the keyboard path: clicking a hint while searching opens
/// detail AND drops the committed query (so Esc from detail closes next press).
#[test]
fn click_from_search_opens_detail_and_clears_query() {
    use crate::actions::ActionId;
    use crate::views::picker::PickerHitAreas;
    use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
    use ratatui::layout::Rect;
    let entries = vec![
        header("Nav", 0, 1),
        hint_with_action("send", key!(Enter), ActionId::SendPrompt),
    ];
    let mut state = build_initial_picker_state(&entries);
    // Active search that still matches the hint row.
    state.set_query("send");
    state.search_active = true;
    // Map a click at row 2 to the hint's position in the filtered view.
    let filtered = filter_entries(&entries, state.query(), false, &no_collapsed());
    let hint_pos = filtered
        .iter()
        .position(|&i| matches!(entries[i], ShortcutsHelpEntry::Hint { .. }))
        .expect("hint present in the filtered view");
    state.hit_areas = Some(PickerHitAreas {
        close_button: Rect::default(),
        search_bar: Rect::default(),
        item_rects: vec![Rect::new(0, 2, 20, 1)],
        entry_indices: vec![hint_pos],
        tab_rects: vec![],
        filter_rect: None,
    });
    let click = MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 1,
        row: 2,
        modifiers: KeyModifiers::NONE,
    };
    let mut mode = browse_mode();
    let result = handle_mouse(
        &click,
        &entries,
        &mut state,
        false,
        &no_collapsed(),
        &mut mode,
    );
    assert_eq!(result, ShortcutsHelpOutcome::Changed);
    assert!(mode.is_detail(), "clicking a hint from search opens detail");
    assert!(
        state.query().is_empty(),
        "click-open detail clears the search query"
    );
    assert!(
        !state.search_active,
        "click-open detail clears search_active"
    );
}

/// The browse footer advertises the detail action so pattern B is discoverable.
#[test]
fn modal_footer_advertises_detail() {
    let footer = modal_footer(false);
    assert!(
        footer.iter().any(|s| s.label.contains("details")),
        "browse footer must advertise Enter details"
    );
}

/// Wiring check: the cheatsheet footer carries the shared `i search` hint
/// under vim and keeps `/ search` regardless. The gate is covered centrally
/// by `modal_window::tests::vim_nav_search_hint_only_in_vim_nav_mode`.
#[test]
fn modal_footer_advertises_i_search_under_vim() {
    let _vim_mode = VimModeGuard::set(true);
    let footer = modal_footer(false);
    assert!(
        footer.iter().any(|s| s.label == "i search"),
        "vim-mode cheatsheet footer must advertise `i search`"
    );
    assert!(
        footer.iter().any(|s| s.label == "/ search"),
        "`/ search` must remain regardless of vim-mode"
    );
}

/// Host path: Enter on a registry hint enters Detail (not Close) via the
/// chrome + picker pipeline both hosts share.
#[test]
fn handle_modal_key_enter_on_hint_enters_detail() {
    use crate::actions::ActionId;
    let entries = vec![
        header("Nav", 0, 1),
        hint_with_action("send", key!(Enter), ActionId::SendPrompt),
    ];
    let mut state = build_initial_picker_state(&entries);
    state.selected = 1;
    let mut window = crate::views::modal_window::ModalWindowState::default();
    let mut mode = browse_mode();
    let outcome = handle_modal_key(
        &make_key(crossterm::event::KeyCode::Enter),
        &entries,
        &mut state,
        &mut window,
        false,
        &no_collapsed(),
        &no_expanded(),
        &mut mode,
        false,
    );
    assert_ne!(
        outcome,
        ModalKeyOutcome::Close,
        "Enter on a hint must not close"
    );
    assert_eq!(outcome, ModalKeyOutcome::Changed);
    assert!(mode.is_detail(), "Enter enters the detail page");
}

/// Over-scrolling a detail body clamps to the last lines instead of paging
/// into an all-blank page.
#[test]
fn render_detail_body_clamps_overscroll() {
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    let theme = crate::theme::Theme::current();
    let area = Rect::new(0, 0, 40, 4);
    let mut buf = Buffer::empty(area);
    // Body taller than the 4-row viewport; a huge offset must still land on the end.
    let body = "L1\nL2\nL3\nL4\nL5\nZqxlast";
    render_detail_body(
        &mut buf,
        area,
        "Title",
        "Enter",
        body,
        false,
        u16::MAX,
        &theme,
    );
    let mut out = String::new();
    for y in area.y..area.y + area.height {
        for x in area.x..area.x + area.width {
            if let Some(cell) = buf.cell((x, y)) {
                out.push_str(cell.symbol());
            }
        }
    }
    assert!(
        out.contains("Zqxlast"),
        "over-scroll must clamp to show the last line, got: {out:?}"
    );
}

/// When the body merely repeats the title (no long_help yet) it must render
/// once; a distinct body (populated long_help) must still render below the title.
#[test]
fn render_detail_body_omits_body_equal_to_title() {
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    let theme = crate::theme::Theme::current();
    let collect = |title: &str, body: &str| -> String {
        let area = Rect::new(0, 0, 40, 10);
        let mut buf = Buffer::empty(area);
        render_detail_body(&mut buf, area, title, "Enter", body, false, 0, &theme);
        let mut out = String::new();
        for y in area.y..area.y + area.height {
            for x in area.x..area.x + area.width {
                if let Some(cell) = buf.cell((x, y)) {
                    out.push_str(cell.symbol());
                }
            }
        }
        out
    };
    assert_eq!(
        collect("Zqxtitle", "Zqxtitle").matches("Zqxtitle").count(),
        1,
        "body equal to title must not be rendered twice"
    );
    let distinct = collect("Zqxtitle", "Zqxbody");
    assert_eq!(distinct.matches("Zqxtitle").count(), 1);
    assert!(
        distinct.contains("Zqxbody"),
        "a distinct body must still render below the title"
    );
}

/// Every action that ships `long_help` carries man-style copy that is present
/// and genuinely distinct from its one-line description. Iterating the whole
/// registry catches a future description-echo on ANY populated action.
#[test]
fn populated_long_help_is_distinct_and_man_style() {
    let registry = ActionRegistry::defaults();
    let populated: Vec<&crate::actions::ActionDef> = registry
        .all()
        .iter()
        .filter(|d| d.long_help.is_some())
        .collect();
    // Sanity floor so an accidental data wipe fails the test rather than passing vacuously.
    assert!(
        populated.len() >= 12,
        "expected the priority long_help set to stay populated, got {}",
        populated.len()
    );
    for def in populated {
        let long = def.long_help.expect("filtered to Some above");
        assert_ne!(
            long, def.description,
            "{:?} long_help must differ from its description (no echo)",
            def.id
        );
        assert!(
            long.contains('\n'),
            "{:?} long_help should be multi-line man-style copy",
            def.id
        );
    }
}

/// `detail_from_entry` surfaces the action's `long_help` as the detail body
/// (not the description), proving the populated copy reaches the screen.
#[test]
fn detail_from_entry_uses_long_help_for_body() {
    let registry = ActionRegistry::defaults();
    let def = registry
        .find(ActionId::ShortcutsHelp)
        .expect("ShortcutsHelp is registered");
    let expected = def.long_help.expect("ShortcutsHelp has long_help");
    let entries = build_entries(&all_contexts(), &registry, true);
    let entry = entries
        .iter()
        .find(|e| hint_expand_action_id(e) == Some(ActionId::ShortcutsHelp))
        .expect("ShortcutsHelp row is present");
    let ShortcutsHelpMode::Detail { body, .. } =
        detail_from_entry(entry).expect("registry hint yields a detail")
    else {
        panic!("expected Detail mode");
    };
    assert_eq!(
        body, expected,
        "detail body must surface the action's long_help"
    );
    assert_ne!(
        body, def.description,
        "detail body must be the long_help, not the description"
    );
}

/// Scroll clamp counts WRAPPED rows: a body that wraps well past the viewport
/// can scroll to its last wrapped row (a logical-line clamp could not reach it).
#[test]
fn render_detail_body_scroll_is_wrap_aware() {
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    let theme = crate::theme::Theme::current();
    // Narrow + short: one logical body line that wraps into many rows.
    let area = Rect::new(0, 0, 20, 4);
    let mut buf = Buffer::empty(area);
    let body = "alpha bravo charlie delta echo foxtrot golf hotel india juliet kilo ZZEND";
    render_detail_body(
        &mut buf,
        area,
        "Title",
        "Enter",
        body,
        false,
        u16::MAX,
        &theme,
    );
    let mut out = String::new();
    for y in area.y..area.y + area.height {
        for x in area.x..area.x + area.width {
            if let Some(cell) = buf.cell((x, y)) {
                out.push_str(cell.symbol());
            }
        }
    }
    assert!(
        out.contains("ZZEND"),
        "wrap-aware clamp must scroll to the last wrapped row, got: {out:?}"
    );
}

/// The detail page (Enter) paints a blank line between paragraphs so wrapped
/// text reads as spaced blocks. The inline expand (arrows) is a separate path
/// and stays tight.
#[test]
fn render_detail_body_spaces_paragraphs_with_blank_line() {
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    let theme = crate::theme::Theme::current();
    let area = Rect::new(0, 0, 40, 8);
    let mut buf = Buffer::empty(area);
    render_detail_body(
        &mut buf,
        area,
        "Title",
        "Enter",
        "First paragraph.\nSecond paragraph.",
        false,
        0,
        &theme,
    );
    let rows: Vec<String> = (area.y..area.y + area.height)
        .map(|y| {
            (area.x..area.x + area.width)
                .filter_map(|x| buf.cell((x, y)).map(|c| c.symbol().to_string()))
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect();
    let first = rows
        .iter()
        .position(|r| r.contains("First paragraph."))
        .expect("first paragraph present");
    let second = rows
        .iter()
        .position(|r| r.contains("Second paragraph."))
        .expect("second paragraph present");
    assert_eq!(
        second,
        first + 2,
        "paragraphs must be separated by exactly one blank row, rows: {rows:?}"
    );
    assert!(
        rows[first + 1].is_empty(),
        "the row between paragraphs must be blank, got {:?}",
        rows[first + 1]
    );
}

/// Search has no long_help — Enter stays in browse.
#[test]
fn enter_on_search_pseudo_row_opens_detail() {
    let registry = ActionRegistry::defaults();
    let entries = build_entries(&all_contexts(), &registry, true);
    let idx = entries
        .iter()
        .position(|e| {
            matches!(
                e,
                ShortcutsHelpEntry::Hint {
                    item,
                    action_id: None,
                    long_help: Some(_),
                    ..
                } if item.label == "search"
            )
        })
        .expect("vim-mode entries include the `/`-search pseudo-row");
    assert_eq!(
        detail_from_entry(&entries[idx])
            .and_then(|m| match m {
                ShortcutsHelpMode::Detail { body, .. } => Some(body),
                _ => None,
            })
            .as_deref(),
        Some(SCROLLBACK_SEARCH_LONG_HELP)
    );
    let mut state = build_initial_picker_state(&entries);
    state.selected = idx;
    let mut mode = browse_mode();
    let out = handle_input(
        &make_key(crossterm::event::KeyCode::Enter),
        &entries,
        &mut state,
        false,
        &no_collapsed(),
        &no_expanded(),
        &mut mode,
    );
    assert_eq!(out, ShortcutsHelpOutcome::Changed);
    assert!(mode.is_detail(), "search pseudo-row Enter opens detail");
}

#[test]
fn enter_on_paste_pseudo_row_opens_detail() {
    let registry = ActionRegistry::defaults();
    let entries = build_entries(&all_contexts(), &registry, true);
    let idx = entries
        .iter()
        .position(|e| {
            matches!(
                e,
                ShortcutsHelpEntry::Hint {
                    item,
                    action_id: None,
                    long_help: Some(_),
                    ..
                } if item.label == "paste"
            )
        })
        .expect("paste pseudo-row with long_help");
    assert_eq!(
        detail_from_entry(&entries[idx])
            .and_then(|m| match m {
                ShortcutsHelpMode::Detail { body, .. } => Some(body),
                _ => None,
            })
            .as_deref(),
        Some(PASTE_LONG_HELP)
    );
    let mut state = build_initial_picker_state(&entries);
    state.selected = idx;
    let mut mode = browse_mode();
    let out = handle_input(
        &make_key(crossterm::event::KeyCode::Enter),
        &entries,
        &mut state,
        false,
        &no_collapsed(),
        &no_expanded(),
        &mut mode,
    );
    assert_eq!(out, ShortcutsHelpOutcome::Changed);
    match &mode {
        ShortcutsHelpMode::Detail {
            body, keys_line, ..
        } => {
            assert_eq!(body, PASTE_LONG_HELP);
            assert!(
                keys_line.to_ascii_lowercase().contains("ctrl+v"),
                "detail keys should list Ctrl+V, got {keys_line:?}"
            );
        }
        ShortcutsHelpMode::Browse => panic!("paste Enter must open detail"),
    }
}

#[test]
fn esc_in_detail_returns_to_browse() {
    let mut mode = ShortcutsHelpMode::Detail {
        title: "Send".into(),
        keys_line: "Enter".into(),
        body: "Send the message".into(),
        dimmed_note: false,
        scroll: 0,
    };
    let entries: Vec<ShortcutsHelpEntry> = vec![];
    let mut state = PickerState::default();
    let result = handle_input(
        &make_key(crossterm::event::KeyCode::Esc),
        &entries,
        &mut state,
        false,
        &no_collapsed(),
        &no_expanded(),
        &mut mode,
    );
    assert_eq!(result, ShortcutsHelpOutcome::Changed);
    assert!(mode.is_browse(), "Esc in detail must return to browse");
}

/// Vim keys (h/j/k/g) are intentionally NOT bound in detail mode — vim modal
/// bindings are owned separately. Arrows/Home scroll; Esc/Left/Backspace go back.
#[test]
fn detail_mode_ignores_vim_keys() {
    use crossterm::event::KeyCode;
    let entries: Vec<ShortcutsHelpEntry> = vec![];
    let mut state = PickerState::default();
    let scroll_of = |m: &ShortcutsHelpMode| match m {
        ShortcutsHelpMode::Detail { scroll, .. } => *scroll,
        _ => u16::MAX,
    };
    let detail = || ShortcutsHelpMode::Detail {
        title: "Send".into(),
        keys_line: "Enter".into(),
        body: "line one\nline two".into(),
        dimmed_note: false,
        scroll: 0,
    };
    // h/j/k/g are inert in detail: no scroll, no back.
    for code in [
        KeyCode::Char('h'),
        KeyCode::Char('j'),
        KeyCode::Char('k'),
        KeyCode::Char('g'),
    ] {
        let mut mode = detail();
        let out = handle_input(
            &make_key(code),
            &entries,
            &mut state,
            false,
            &no_collapsed(),
            &no_expanded(),
            &mut mode,
        );
        assert!(mode.is_detail(), "{code:?} must not leave detail mode");
        assert_eq!(
            scroll_of(&mode),
            0,
            "{code:?} must not scroll the detail body"
        );
        assert_eq!(
            out,
            ShortcutsHelpOutcome::Unchanged,
            "{code:?} must be inert in detail, got {out:?}"
        );
    }
    // Non-vim keys still work: Down scrolls, Left returns to browse.
    let mut mode = detail();
    let _ = handle_input(
        &make_key(KeyCode::Down),
        &entries,
        &mut state,
        false,
        &no_collapsed(),
        &no_expanded(),
        &mut mode,
    );
    assert_eq!(scroll_of(&mode), 1, "Down scrolls the detail body");
    let _ = handle_input(
        &make_key(KeyCode::Left),
        &entries,
        &mut state,
        false,
        &no_collapsed(),
        &no_expanded(),
        &mut mode,
    );
    assert!(mode.is_browse(), "Left returns to browse");
}

/// Host path: chrome must not intercept Esc while in detail (would close the
/// modal); it returns to browse and keeps the modal open.
#[test]
fn handle_modal_key_esc_in_detail_is_back_not_close() {
    let entries = vec![header("Nav", 0, 1), hint("send", key!(Enter))];
    let mut state = build_initial_picker_state(&entries);
    let mut window = crate::views::modal_window::ModalWindowState::default();
    let collapsed = no_collapsed();
    let mut mode = ShortcutsHelpMode::Detail {
        title: "Send".into(),
        keys_line: "Enter".into(),
        body: "Send the message".into(),
        dimmed_note: false,
        scroll: 0,
    };
    let outcome = handle_modal_key(
        &make_key(crossterm::event::KeyCode::Esc),
        &entries,
        &mut state,
        &mut window,
        false,
        &collapsed,
        &no_expanded(),
        &mut mode,
        false,
    );
    assert_ne!(
        outcome,
        ModalKeyOutcome::Close,
        "Esc in detail must not close"
    );
    assert_eq!(outcome, ModalKeyOutcome::Changed);
    assert!(mode.is_browse(), "Esc in detail returns to browse");
}

#[test]
fn esc_in_browse_closes_via_picker() {
    let entries = vec![header("Nav", 0, 1), hint("send", key!(Enter))];
    let mut state = build_initial_picker_state(&entries);
    let mut mode = browse_mode();
    let result = handle_input(
        &make_key(crossterm::event::KeyCode::Esc),
        &entries,
        &mut state,
        false,
        &no_collapsed(),
        &no_expanded(),
        &mut mode,
    );
    assert_eq!(result, ShortcutsHelpOutcome::Close);
    assert!(mode.is_browse());
}

#[test]
fn ctrl_dot_closes_from_detail_mode() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let mut mode = ShortcutsHelpMode::Detail {
        title: "Send".into(),
        keys_line: "Enter".into(),
        body: "body".into(),
        dimmed_note: false,
        scroll: 3,
    };
    let entries: Vec<ShortcutsHelpEntry> = vec![];
    let mut state = PickerState::default();
    let key = KeyEvent::new(KeyCode::Char('.'), KeyModifiers::CONTROL);
    let result = handle_input(
        &key,
        &entries,
        &mut state,
        false,
        &no_collapsed(),
        &no_expanded(),
        &mut mode,
    );
    assert_eq!(result, ShortcutsHelpOutcome::Close);
    // mode is unchanged by handle_input; caller clears the modal.
    assert!(mode.is_detail());
}

#[test]
fn ctrl_x_closes_from_browse_mode() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let entries = vec![header("Nav", 0, 1), hint("send", key!(Enter))];
    let mut state = build_initial_picker_state(&entries);
    let mut mode = browse_mode();
    let key = KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL);
    let result = handle_input(
        &key,
        &entries,
        &mut state,
        false,
        &no_collapsed(),
        &no_expanded(),
        &mut mode,
    );
    assert_eq!(result, ShortcutsHelpOutcome::Close);
}

#[test]
fn vim_i_enters_search_and_printables_type_afterward() {
    let _vim_mode = VimModeGuard::set(true);
    let (entries, mut state) = setup_on_header();
    assert!(!state.search_active);
    let mut mode = browse_mode();
    let enter_search = handle_input(
        &make_key(crossterm::event::KeyCode::Char('i')),
        &entries,
        &mut state,
        false,
        &no_collapsed(),
        &no_expanded(),
        &mut mode,
    );
    assert_eq!(enter_search, ShortcutsHelpOutcome::Changed);
    assert!(state.search_active, "`i` must activate cheatsheet search");
    assert!(state.query().is_empty(), "`i` must not enter search text");

    let type_j = handle_input(
        &make_key(crossterm::event::KeyCode::Char('j')),
        &entries,
        &mut state,
        false,
        &no_collapsed(),
        &no_expanded(),
        &mut mode,
    );
    assert_eq!(type_j, ShortcutsHelpOutcome::Changed);
    assert_eq!(state.query(), "j", "printables must type in active search");
}

// ── vim_mode tests ───────────────────────────────────────────

#[test]
fn vim_mode_jk_navigate_without_starting_search() {
    let _vim_mode = VimModeGuard::set(true);
    let entries = vec![
        header("Nav", 0, 3),
        hint("send", key!(Enter)),
        hint("next", key!('n')),
        hint("quit", key!('q', CONTROL)),
    ];
    let mut state = build_initial_picker_state(&entries);
    let mut mode = browse_mode();

    let down = handle_input(
        &make_key(crossterm::event::KeyCode::Char('j')),
        &entries,
        &mut state,
        false,
        &no_collapsed(),
        &no_expanded(),
        &mut mode,
    );
    assert_eq!(down, ShortcutsHelpOutcome::Changed);
    assert_eq!(state.selected, 2, "`j` must select the next row");
    assert!(state.query().is_empty(), "`j` must not enter search text");
    assert!(!state.search_active, "`j` must leave search inactive");

    let up = handle_input(
        &make_key(crossterm::event::KeyCode::Char('k')),
        &entries,
        &mut state,
        false,
        &no_collapsed(),
        &no_expanded(),
        &mut mode,
    );
    assert_eq!(up, ShortcutsHelpOutcome::Changed);
    assert_eq!(state.selected, 1, "`k` must select the previous row");
    assert!(state.query().is_empty(), "`k` must not enter search text");
    assert!(!state.search_active, "`k` must leave search inactive");
}

#[test]
fn non_vim_hjkl_start_search() {
    let _vim_mode = VimModeGuard::set(false);
    let (entries, state) = setup_on_header();

    for ch in ['h', 'j', 'k', 'l'] {
        let mut state = state.clone();
        let collapsed = if ch == 'l' {
            std::collections::HashSet::from([0])
        } else {
            no_collapsed()
        };
        let mut mode = browse_mode();
        let result = handle_input(
            &make_key(crossterm::event::KeyCode::Char(ch)),
            &entries,
            &mut state,
            false,
            &collapsed,
            &no_expanded(),
            &mut mode,
        );
        assert_eq!(
            result,
            ShortcutsHelpOutcome::Changed,
            "non-vim `{ch}` must start search"
        );
        assert_eq!(state.query(), ch.to_string(), "non-vim `{ch}` must type");
    }
}

/// In non-vim mode, `j/k` row should drop the `j` key and show only
/// the `Down` alt — `Down` still works and the row should not be dimmed.
#[test]
fn build_entries_vim_off_keeps_arrow_alt_without_vim_key() {
    let registry = ActionRegistry::defaults();
    let entries = build_entries(&all_contexts(), &registry, false);

    let nav = entries
        .iter()
        .find_map(|e| match e {
            ShortcutsHelpEntry::Hint { item, dimmed, .. } if item.label == "nav" => {
                Some((item, *dimmed))
            }
            _ => None,
        })
        .expect("nav (SelectNext) row should be present in non-vim mode");
    let (item, dimmed) = nav;
    assert!(!dimmed, "nav row with Down alt should not be dimmed");
    assert!(
        item.keys.iter().all(|k| !k.is_letter_or_shift_letter()),
        "non-vim cheatsheet must not advertise letter keys; got {:?}",
        item.keys.iter().map(|k| k.display()).collect::<Vec<_>>()
    );
    assert!(
        !item.keys.is_empty(),
        "row must retain at least one (non-vim) key"
    );
}

/// In non-vim mode, scrollback bindings that have NO non-vim alt
/// (e.g. `g` GotoTop, `y` CopyBlockContent) should be hidden from the
/// cheatsheet entirely.
#[test]
fn build_entries_vim_off_hides_vim_only_rows() {
    let registry = ActionRegistry::defaults();
    let entries = build_entries(&all_contexts(), &registry, false);

    for label in ["top", "btm", "copy", "copy cmd"] {
        let present = entries.iter().any(|e| {
            matches!(
                e,
                ShortcutsHelpEntry::Hint { item, .. } if item.label == label
            )
        });
        assert!(
            !present,
            "{label:?} (vim-only) should be hidden from cheatsheet when vim_mode=false"
        );
    }
}

/// Vim mode ON: both vim key and arrow alt should be visible on the
/// same row.
#[test]
fn build_entries_vim_on_shows_both_vim_and_arrow_keys() {
    let registry = ActionRegistry::defaults();
    let entries = build_entries(&all_contexts(), &registry, true);

    let nav_keys: Vec<String> = entries
        .iter()
        .find_map(|e| match e {
            ShortcutsHelpEntry::Hint { item, .. } if item.label == "nav" => {
                Some(item.keys.iter().map(|k| k.display().to_string()).collect())
            }
            _ => None,
        })
        .expect("nav row should be present in vim mode");
    let nav_keys_joined = nav_keys.join(" ");
    assert!(
        nav_keys_joined.contains('j') || nav_keys_joined.contains('J'),
        "vim mode should show `j` key for nav: {nav_keys:?}"
    );
    assert!(
        nav_keys_joined.contains('↓') || nav_keys_joined.to_lowercase().contains("down"),
        "vim mode should also show arrow alt: {nav_keys:?}"
    );
}

/// Asserts that the cheatsheet row for `label` advertises `expected_key`
/// (primary or alt). Used by the Windows-fallback regressions below.
fn assert_cheatsheet_row_has_key(entries: &[ShortcutsHelpEntry], label: &str, expected_key: &str) {
    let keys: Vec<String> = entries
        .iter()
        .find_map(|e| match e {
            ShortcutsHelpEntry::Hint { item, .. } if item.label == label => {
                Some(item.keys.iter().map(|k| k.display()).collect())
            }
            _ => None,
        })
        .unwrap_or_else(|| panic!("{label:?} row not found in cheatsheet"));
    assert!(
        keys.iter().any(|k| k == expected_key),
        "{label} cheatsheet row missing {expected_key}; got {keys:?}"
    );
}

#[test]
fn build_entries_surfaces_interject_ctrl_i_fallback() {
    let registry = ActionRegistry::defaults();
    let entries = build_entries(&all_contexts(), &registry, true);
    // Action label is compact "send now" wording (interject under the hood).
    assert_cheatsheet_row_has_key(&entries, "send now", "Ctrl+i");
}

#[test]
fn build_entries_surfaces_queue_ctrl_apostrophe_fallback() {
    let registry = ActionRegistry::defaults();
    let entries = build_entries(&all_contexts(), &registry, true);
    assert_cheatsheet_row_has_key(&entries, "queue", "Ctrl+'");
}

/// A section whose entries are all filtered out should have its
/// header dropped, not rendered as a dead row.
#[test]
fn build_entries_vim_off_drops_empty_section_headers() {
    let registry = ActionRegistry::defaults();
    let entries = build_entries(&all_contexts(), &registry, false);

    for entry in &entries {
        if let ShortcutsHelpEntry::SectionHeader {
            entry_count, label, ..
        } = entry
        {
            assert!(
                *entry_count > 0,
                "section {label:?} has 0 entries — should have been dropped"
            );
        }
    }
}

#[test]
fn build_entries_sets_action_id_on_registry_hints() {
    let registry = ActionRegistry::defaults();
    let entries = build_entries(&all_contexts(), &registry, true);
    let shortcuts_id = entries.iter().find_map(|e| match e {
        ShortcutsHelpEntry::Hint {
            item,
            action_id: Some(id),
            ..
        } if item.description.as_deref() == Some("Keyboard shortcuts") => Some(*id),
        _ => None,
    });
    assert_eq!(
        shortcuts_id,
        Some(ActionId::ShortcutsHelp),
        "registry-backed hints must carry their ActionId for expand/detail"
    );

    // Registry rows carry ActionId; known display-only rows stay action-less.
    let search_key = key!('/');
    let paste_key = key!('v', CONTROL);
    let undo_key = key!('z', CONTROL);
    let redo_key = key!('z', CONTROL | SHIFT);
    // Prompt history (Up / /history) is an inline key handler + slash
    // command, not an ActionRegistry entry, so it stays display-only too.
    let history_key = key!(Up);
    for entry in &entries {
        let ShortcutsHelpEntry::Hint {
            item, action_id, ..
        } = entry
        else {
            continue;
        };
        let is_pseudo = match item.label.as_ref() {
            "search" => item.keys.contains(&search_key),
            "paste" => item.keys.contains(&paste_key),
            "undo" => item.keys.contains(&undo_key),
            "redo" => item.keys.contains(&redo_key),
            "history" => item.keys.contains(&history_key),
            _ => false,
        };
        if is_pseudo {
            assert!(
                action_id.is_none(),
                "pseudo-row {:?} must stay display-only",
                item.label
            );
        } else {
            assert!(
                action_id.is_some(),
                "registry-backed hint {:?} lost its action_id",
                item.label
            );
        }
    }
}

#[test]
fn toggle_expand_outcome_for_hint_right_key() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let registry = ActionRegistry::defaults();
    let entries = build_entries(&all_contexts(), &registry, true);
    let mut state = build_initial_picker_state(&entries);
    // Select first non-header row (Essentials section is first header at 0).
    state.selected = 1;
    let mut mode = browse_mode();
    let key = KeyEvent::new(KeyCode::Right, KeyModifiers::NONE);
    let out = handle_input(
        &key,
        &entries,
        &mut state,
        false,
        &no_collapsed(),
        &no_expanded(),
        &mut mode,
    );
    assert!(
        matches!(out, ShortcutsHelpOutcome::ToggleExpand(_)),
        "Right on hint row should toggle inline expand, got {out:?}"
    );
    let left = KeyEvent::new(KeyCode::Left, KeyModifiers::NONE);
    let collapsed = handle_input(
        &left,
        &entries,
        &mut state,
        false,
        &no_collapsed(),
        &no_expanded(),
        &mut mode,
    );
    assert!(
        !matches!(collapsed, ShortcutsHelpOutcome::ToggleExpand(_)),
        "Left on a collapsed hint must be inert, got {collapsed:?}"
    );
    assert!(mode.is_browse(), "Left must not leave browse mode");
    let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
    let out_enter = handle_input(
        &enter,
        &entries,
        &mut state,
        false,
        &no_collapsed(),
        &no_expanded(),
        &mut mode,
    );
    assert_eq!(
        out_enter,
        ShortcutsHelpOutcome::Changed,
        "Enter on a registry hint opens detail (pattern B), got {out_enter:?}"
    );
    assert!(mode.is_detail(), "Enter switches mode to Detail directly");
}

#[test]
fn vim_h_collapses_only_expanded_action_hints() {
    use crate::actions::ActionId;
    let _vim_mode = VimModeGuard::set(true);
    let entries = vec![
        header("Nav", 0, 1),
        hint_with_action("send", key!(Enter), ActionId::SendPrompt),
    ];
    let mut state = build_initial_picker_state(&entries);
    state.selected = 1;
    let mut mode = browse_mode();

    let collapsed = handle_input(
        &make_key(crossterm::event::KeyCode::Char('h')),
        &entries,
        &mut state,
        false,
        &no_collapsed(),
        &no_expanded(),
        &mut mode,
    );
    assert_eq!(
        collapsed,
        ShortcutsHelpOutcome::Unchanged,
        "vim h on a collapsed action hint must be inert"
    );
    assert!(state.query().is_empty(), "vim h must not enter search text");

    let key_id = ExpandKey::Action(ActionId::SendPrompt);
    let expanded = std::collections::HashSet::from([key_id]);
    let collapse = handle_input(
        &make_key(crossterm::event::KeyCode::Char('h')),
        &entries,
        &mut state,
        false,
        &no_collapsed(),
        &expanded,
        &mut mode,
    );
    assert_eq!(
        collapse,
        ShortcutsHelpOutcome::ToggleExpand(key_id),
        "vim h must collapse an expanded action hint"
    );
    assert!(state.query().is_empty(), "vim h must not enter search text");
}

#[test]
fn search_pseudo_row_expands() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let registry = ActionRegistry::defaults();
    let entries = build_entries(&all_contexts(), &registry, true);
    let search_idx = entries
        .iter()
        .position(|e| {
            matches!(
                e,
                ShortcutsHelpEntry::Hint { item, action_id: None, .. }
                    if item.label == "search"
            )
        })
        .expect("vim-mode entries include the `/`-search pseudo-row");
    for code in [KeyCode::Right, KeyCode::Char('e'), KeyCode::Char(' ')] {
        let mut state = build_initial_picker_state(&entries);
        state.selected = search_idx;
        let key = KeyEvent::new(code, KeyModifiers::NONE);
        let mut mode = ShortcutsHelpMode::Browse;
        let out = handle_input(
            &key,
            &entries,
            &mut state,
            false,
            &no_collapsed(),
            &no_expanded(),
            &mut mode,
        );
        assert_eq!(
            out,
            ShortcutsHelpOutcome::ToggleExpand(ExpandKey::Pseudo("search")),
            "search pseudo-row must expand for {code:?}, got {out:?}"
        );
    }
}

#[test]
fn vim_l_expands_and_h_collapses_paste() {
    use crossterm::event::KeyCode;
    let _vim_mode = VimModeGuard::set(true);
    let registry = ActionRegistry::defaults();
    let entries = build_entries(&all_contexts(), &registry, true);
    let paste_idx = entries
        .iter()
        .position(|e| {
            matches!(
                e,
                ShortcutsHelpEntry::Hint {
                    item,
                    action_id: None,
                    long_help: Some(_),
                    ..
                } if item.label == "paste"
            )
        })
        .expect("paste pseudo-row with long_help");
    let key_id = ExpandKey::Pseudo("paste");
    assert_eq!(expand_key(&entries[paste_idx]), Some(key_id));
    let mut state = build_initial_picker_state(&entries);
    state.selected = paste_idx;
    let mut mode = ShortcutsHelpMode::Browse;
    let expand = handle_input(
        &make_key(KeyCode::Char('l')),
        &entries,
        &mut state,
        false,
        &no_collapsed(),
        &no_expanded(),
        &mut mode,
    );
    assert_eq!(
        expand,
        ShortcutsHelpOutcome::ToggleExpand(key_id),
        "vim l must expand the paste pseudo-row"
    );
    assert!(state.query().is_empty(), "vim l must not enter search text");

    let expanded = std::collections::HashSet::from([key_id]);
    let collapse = handle_input(
        &make_key(KeyCode::Char('h')),
        &entries,
        &mut state,
        false,
        &no_collapsed(),
        &expanded,
        &mut mode,
    );
    assert_eq!(
        collapse,
        ShortcutsHelpOutcome::ToggleExpand(key_id),
        "vim h must collapse the expanded paste pseudo-row"
    );
    assert!(state.query().is_empty(), "vim h must not enter search text");
}

/// `handle_modal_key` (chrome + picker pipeline) maps the hint-row expand to
/// `ModalKeyOutcome::ToggleExpand` so dashboards get identical semantics.
#[test]
fn handle_modal_key_maps_toggle_expand() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let registry = ActionRegistry::defaults();
    let entries = build_entries(&all_contexts(), &registry, true);
    let mut state = build_initial_picker_state(&entries);
    state.selected = 1;
    let mut window = crate::views::modal_window::ModalWindowState::default();
    let key = KeyEvent::new(KeyCode::Right, KeyModifiers::NONE);
    let mut mode = ShortcutsHelpMode::Browse;
    let out = handle_modal_key(
        &key,
        &entries,
        &mut state,
        &mut window,
        false,
        &no_collapsed(),
        &no_expanded(),
        &mut mode,
        false,
    );
    assert!(
        matches!(out, ModalKeyOutcome::ToggleExpand(_)),
        "Right on a hint row must map to ModalKeyOutcome::ToggleExpand, got {out:?}"
    );
}

/// `handle_modal_key` forwards `expanded_ids` through the chrome pipeline so
/// the dashboard host's Left-collapse works. A *populated* expanded set is
/// required to exercise the wiring — the `→` test above passes regardless of
/// the set, so it can't catch a dropped `expanded_ids` forward.
#[test]
fn handle_modal_key_left_collapses_expanded_hint() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let registry = ActionRegistry::defaults();
    let entries = build_entries(&all_contexts(), &registry, true);
    let mut state = build_initial_picker_state(&entries);
    state.selected = 1;
    let key_id = expand_key(&entries[1]).expect("row 1 is expandable");
    let expanded = std::collections::HashSet::from([key_id]);
    let mut window = crate::views::modal_window::ModalWindowState::default();
    let key = KeyEvent::new(KeyCode::Left, KeyModifiers::NONE);
    let mut mode = ShortcutsHelpMode::Browse;
    let out = handle_modal_key(
        &key,
        &entries,
        &mut state,
        &mut window,
        false,
        &no_collapsed(),
        &expanded,
        &mut mode,
        false,
    );
    assert_eq!(
        out,
        ModalKeyOutcome::ToggleExpand(key_id),
        "Left on an expanded hint must map to ModalKeyOutcome::ToggleExpand (collapse), got {out:?}"
    );
}

/// A row's `long_help` renders as an inline line only while its id is
/// expanded, and is absent otherwise.
#[test]
fn render_modal_shows_long_help_only_when_expanded() {
    use crate::actions::ActionId;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;

    // long_help differs from label/description so the expanded line is detectable.
    let mut item = HintItem::new(key!('q', CONTROL), "quit");
    item.description = Some("Quit the app".into());
    let entries = vec![
        ShortcutsHelpEntry::SectionHeader {
            label: "Essentials",
            category_idx: 0,
            entry_count: 1,
        },
        ShortcutsHelpEntry::Hint {
            item,
            dimmed: false,
            action_id: Some(ActionId::Quit),
            long_help: Some("Zqxhelpline"),
        },
    ];
    let theme = crate::theme::Theme::current();
    let area = Rect::new(0, 0, 100, 40);
    let render = |expanded: &std::collections::HashSet<ExpandKey>| -> String {
        let mut state = build_initial_picker_state(&entries);
        let mut window = crate::views::modal_window::ModalWindowState::default();
        let mut buf = Buffer::empty(area);
        render_modal(
            &mut buf,
            area,
            &entries,
            &mut state,
            &mut window,
            false,
            &no_collapsed(),
            expanded,
            &ShortcutsHelpMode::Browse,
            &theme,
            false,
        );
        let mut out = String::new();
        for y in area.y..area.y + area.height {
            for x in area.x..area.x + area.width {
                if let Some(cell) = buf.cell((x, y)) {
                    out.push_str(cell.symbol());
                }
            }
        }
        out
    };
    let mut expanded = std::collections::HashSet::new();
    expanded.insert(ExpandKey::Action(ActionId::Quit));
    assert!(
        render(&expanded).contains("Zqxhelpline"),
        "expanded hint must render its long_help line"
    );
    assert!(
        !render(&std::collections::HashSet::new()).contains("Zqxhelpline"),
        "collapsed hint must not render the long_help line"
    );
}

/// The collapsible (inline expand) view collapses newlines to spaces so the
/// help renders as one wrap-flowed block with no hard breaks — unlike the
/// detail page (Enter), which spaces paragraphs out with blank lines.
#[test]
fn cheatsheet_rows_inline_help_joins_newlines_with_spaces() {
    use crate::actions::ActionId;
    let mut item = HintItem::new(key!('q', CONTROL), "quit");
    item.description = Some("Quit the app".into());
    let entries = vec![
        ShortcutsHelpEntry::SectionHeader {
            label: "Essentials",
            category_idx: 0,
            entry_count: 1,
        },
        ShortcutsHelpEntry::Hint {
            item,
            dimmed: false,
            action_id: Some(ActionId::Quit),
            long_help: Some("First line.\nSecond line."),
        },
    ];
    let rows = CheatsheetRows::build(&entries, "", false, &no_collapsed());
    let help = rows.help_refs();
    assert_eq!(
        help[1], "First line. Second line.",
        "inline help must join newlines with spaces"
    );
    assert!(
        !help[1].contains('\n'),
        "collapsible help must not contain newlines, got {:?}",
        help[1]
    );
}

/// A hint with neither long_help nor description has empty inline help, so an
/// expanded row must render no description line (no stray blank inline row).
#[test]
fn inline_expand_with_no_help_renders_no_description_line() {
    use crate::actions::ActionId;
    use crate::views::picker::PickerEntry;
    // HintItem::new leaves `description` unset; no long_help either.
    let item = HintItem::new(key!('q', CONTROL), "quit");
    let entries = vec![
        ShortcutsHelpEntry::SectionHeader {
            label: "Essentials",
            category_idx: 0,
            entry_count: 1,
        },
        ShortcutsHelpEntry::Hint {
            item,
            dimmed: false,
            action_id: Some(ActionId::Quit),
            long_help: None,
        },
    ];
    let rows = CheatsheetRows::build(&entries, "", false, &no_collapsed());
    let help = rows.help_refs();
    assert_eq!(
        help[1], "",
        "a hint with no help source has empty inline help"
    );
    let mut state = build_initial_picker_state(&entries);
    state.selected = 1;
    let expanded = std::collections::HashSet::from([ExpandKey::Action(ActionId::Quit)]);
    let picker_entries = rows.picker_entries(&state, &expanded, &help);
    let PickerEntry::Row(row) = &picker_entries[1] else {
        panic!("row 1 must be a hint row");
    };
    assert!(row.expanded, "row is expanded");
    assert!(
        row.description_lines.is_empty(),
        "empty help must render no description line even when expanded"
    );
}
