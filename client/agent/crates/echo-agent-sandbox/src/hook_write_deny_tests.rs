use super::*;

#[test]
fn revalidate_refuses_replaced_directory() {
    let root = std::env::temp_dir().join(format!(
        "echo-agent-id-race-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let hooks = root.join("hooks");
    std::fs::create_dir_all(&hooks).unwrap();
    let id = capture_path_identity(&hooks).unwrap();
    revalidate_path_identity(&id).unwrap();

    let moved = root.join("hooks-old");
    std::fs::rename(&hooks, &moved).unwrap();
    std::fs::create_dir_all(&hooks).unwrap();

    let err = revalidate_path_identity(&id).unwrap_err();
    assert!(
        matches!(err, HookWriteDenyError::IdentityChanged { .. }),
        "expected IdentityChanged, got {err:?}"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn revalidate_refuses_symlink_swap() {
    let root = std::env::temp_dir().join(format!(
        "echo-agent-id-symlink-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let hooks = root.join("hooks");
    std::fs::create_dir_all(&hooks).unwrap();
    let id = capture_path_identity(&hooks).unwrap();

    let moved = root.join("hooks-old");
    std::fs::rename(&hooks, &moved).unwrap();
    std::os::unix::fs::symlink(&moved, &hooks).unwrap();

    let err = revalidate_path_identity(&id).unwrap_err();
    assert!(
        matches!(
            err,
            HookWriteDenyError::Symlink { .. } | HookWriteDenyError::IdentityChanged { .. }
        ),
        "expected symlink/identity error, got {err:?}"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn capture_refuses_hardlinked_regular_file() {
    let root = std::env::temp_dir().join(format!(
        "echo-agent-hardlink-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let reg = root.join("hooks-paths");
    let alias = root.join("hooks-paths-alias");
    std::fs::write(&reg, b"").unwrap();
    std::fs::hard_link(&reg, &alias).unwrap();

    let err = capture_path_identity(&reg).unwrap_err();
    assert!(
        matches!(err, HookWriteDenyError::HardLink { nlink, .. } if nlink >= 2),
        "expected HardLink, got {err:?}"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
#[cfg(target_os = "linux")]
fn revalidate_rejects_late_json_file_after_plan_capture() {
    let root = std::env::temp_dir().join(format!(
        "echo-agent-late-json-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let hooks = root.join("hooks");
    std::fs::create_dir_all(&hooks).unwrap();
    std::fs::write(hooks.join("keep.json"), b"{}").unwrap();
    let sources = [GlobalHookSource {
        path: hooks.clone(),
        kind: echo_agent_config::GlobalHookSourceKind::HookDirectory,
    }];
    let plan = build_bwrap_plan(&sources).expect("plan");
    revalidate_plan(&plan).expect("stable");

    // Late insert after capture (hardlinked alias also exercises nlink).
    let late = hooks.join("late.json");
    let alias = root.join("late-alias.json");
    std::fs::write(&late, b"{}").unwrap();
    std::fs::hard_link(&late, &alias).unwrap();

    let err = revalidate_plan(&plan).unwrap_err();
    // Late hardlinked JSON may surface as Resolve (config validation wrapped via From)
    // before a typed HardLink/JsonSnapshotChanged, depending on check order.
    assert!(
        matches!(
            err,
            HookWriteDenyError::JsonSnapshotChanged { .. }
                | HookWriteDenyError::HardLink { .. }
                | HookWriteDenyError::Resolve(_)
        ),
        "expected snapshot/hardlink/resolve failure, got {err:?}"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn hardlinked_discovery_json_under_hooks_dir_refused() {
    let root = std::env::temp_dir().join(format!(
        "echo-agent-hl-json-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let hooks = root.join("hooks");
    std::fs::create_dir_all(&hooks).unwrap();
    let active = hooks.join("active.json");
    let alias = hooks.join("alias.json");
    std::fs::write(&active, b"{}").unwrap();
    std::fs::hard_link(&active, &alias).unwrap();
    let sources = [GlobalHookSource {
        path: hooks,
        kind: echo_agent_config::GlobalHookSourceKind::HookDirectory,
    }];
    let err = echo_agent_config::validated_hook_json_files_for_sources(&sources).unwrap_err();
    assert!(matches!(
        err,
        echo_agent_config::GlobalHookSourceError::HardLinkedHookFile { .. }
    ));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn reject_hardlinked_files_on_registry_source() {
    let root = std::env::temp_dir().join(format!(
        "echo-agent-hl-src-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(root.join("hooks")).unwrap();
    let reg = root.join("hooks-paths");
    let alias = root.join("alias");
    std::fs::write(&reg, b"").unwrap();
    std::fs::hard_link(&reg, &alias).unwrap();

    let sources = [GlobalHookSource {
        path: reg,
        kind: echo_agent_config::GlobalHookSourceKind::RegistryFile,
    }];
    let err = reject_hardlinked_files(&sources).unwrap_err();
    assert!(matches!(err, HookWriteDenyError::HardLink { .. }));
    let _ = std::fs::remove_dir_all(&root);
}
