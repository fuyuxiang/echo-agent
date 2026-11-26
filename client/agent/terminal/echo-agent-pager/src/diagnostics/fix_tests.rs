use super::*;
use crate::clipboard::{ClipboardDelivery, NativeClipboardPreflight, Osc52Capability};
use crate::diagnostics::{DiagnosticFinding, FindingDisposition, ManualRemediation};
use crate::host::DisplayServer;
use crate::terminal::{MultiplexerKind, TerminalName};

pub(super) fn report() -> DiagnosticReport {
    let mut report = DiagnosticReport {
        facts: crate::diagnostics::DiagnosticFacts {
            terminal: TerminalName::Ghostty,
            xtversion: crate::diagnostics::RuntimeFact::Unavailable,
            multiplexer: MultiplexerKind::Undetected,
            byobu: None,
            ssh: false,
            tmux: crate::diagnostics::TmuxFacts {
                extended_keys: crate::diagnostics::TmuxOptionFact::Unavailable,
                set_clipboard: crate::diagnostics::TmuxOptionFact::Unavailable,
                allow_passthrough_support: crate::diagnostics::TmuxSupportFact::Unavailable,
                allow_passthrough: crate::diagnostics::TmuxOptionFact::Unavailable,
                color_passthrough: crate::diagnostics::TmuxColorPassthrough::Unknown,
            },
            color: crate::diagnostics::ColorFacts {
                level: crate::diagnostics::RuntimeFact::Unavailable,
                available_themes: Vec::new(),
                total_themes: crate::theme::ThemeKind::ALL.len(),
            },
            keyboard: None,
            newline: None,
            clipboard: crate::diagnostics::ClipboardFacts {
                native_route: false,
                native_tool: "none".to_owned(),
                native_preflight: NativeClipboardPreflight::Disabled,
                tmux_route: false,
                osc52_route: false,
                osc52_capability: Osc52Capability::Unknown,
                wrap_sink: false,
                display_server: DisplayServer::Unknown,
                container_no_display: false,
                data_control: crate::diagnostics::DataControlFact::NotApplicable,
                delivery: ClipboardDelivery::Failed,
                fix: None,
            },
            voice: None,
        },
        findings: Vec::new(),
        probe_notes: Vec::new(),
    };
    report.findings.push(DiagnosticFinding {
        id: SSH_WRAP_ID,
        disposition: FindingDisposition::Recommendation,
        message: "Use local SSH wrapping".to_owned(),
        remediation: Some(ManualRemediation {
            fix: SSH_WRAP_ONE_OFF.to_owned(),
            config_path: None,
        }),
        automatic_remediation: Some(ssh_wrap_automatic_remediation()),
        note: None,
    });
    report
}

fn terminal() -> TerminalContext {
    TerminalContext {
        brand: TerminalName::Ghostty,
        env_brand: TerminalName::Ghostty,
        multiplexer: MultiplexerKind::Undetected,
        byobu: None,
        embedded_editor: None,
        tmux_meta: Default::default(),
        is_ssh: false,
        is_official_vscode_remote: false,
        term_var: Some("xterm-256color".to_owned()),
        tmux_version: None,
        vte_version: None,
        tmux_extended_keys: None,
        term_program_version: None,
        env_term_version: None,
    }
}

pub(super) fn request(home: &Path, shell: &str) -> FixRequest {
    FixRequest::new_for_test(SSH_WRAP_ID, home, Some(PathBuf::from(shell)), None, None).unwrap()
}

#[test]
fn canonical_and_short_ids_resolve_to_canonical_id() {
    assert_eq!(resolve_fix_id("terminal.ssh-wrap").unwrap(), SSH_WRAP_ID);
    let command = human_fix_command(SSH_WRAP_ID).expect("SSH fix command");
    assert_eq!(command, "echoagent doctor fix ssh-wrap");
    assert_eq!(
        resolve_fix_id(command.strip_prefix("echoagent doctor fix ").unwrap()).unwrap(),
        SSH_WRAP_ID
    );
    assert!(human_fix_command(DiagnosticId::new("terminal", "unknown")).is_none());
    assert!(matches!(
        resolve_fix_id("terminal.unknown"),
        Err(FixError::UnknownId(_))
    ));
}

#[test]
fn applicable_fix_listing_uses_report_metadata_and_planner_availability() {
    let temp = tempfile::tempdir().unwrap();
    let report = report();
    let local = terminal();
    let local_fixes = applicable_automatic_fixes_with(&report, &local, |id| {
        FixRequest::new_for_test(
            id,
            temp.path(),
            Some(PathBuf::from("/bin/bash")),
            None,
            None,
        )
    });
    assert_eq!(
        local_fixes,
        vec![(SSH_WRAP_ID, "ssh-wrap", AutomaticFixAvailability::Here)]
    );

    let mut remote = local.clone();
    remote.is_ssh = true;
    assert_eq!(
        applicable_automatic_fixes_with(&report, &remote, |_| { Err(FixError::HomeUnavailable) }),
        vec![(
            SSH_WRAP_ID,
            "ssh-wrap",
            AutomaticFixAvailability::RunLocally
        )]
    );

    let mut manual_only = report;
    manual_only.findings[0].automatic_remediation = None;
    assert!(
        applicable_automatic_fixes_with(&manual_only, &local, |_| {
            Err(FixError::HomeUnavailable)
        })
        .is_empty()
    );
}

fn tmux_terminal(byobu: bool) -> TerminalContext {
    TerminalContext {
        multiplexer: MultiplexerKind::Tmux,
        byobu: byobu.then_some(crate::terminal::ByobuBackend::Tmux),
        tmux_version: Some("tmux 3.4".to_owned()),
        tmux_extended_keys: Some("off".to_owned()),
        ..terminal()
    }
}

fn tmux_report(id: DiagnosticId, evidence: TmuxEvidence) -> DiagnosticReport {
    let mut report = report();
    report.findings.clear();
    report.facts.multiplexer = MultiplexerKind::Tmux;
    report.facts.tmux = crate::diagnostics::TmuxFacts {
        extended_keys: crate::diagnostics::TmuxOptionFact::Available(
            if evidence == TmuxEvidence::ExtendedKeys {
                "off"
            } else {
                "on"
            }
            .to_owned(),
        ),
        set_clipboard: crate::diagnostics::TmuxOptionFact::Available(
            if evidence == TmuxEvidence::Clipboard {
                "off"
            } else {
                "on"
            }
            .to_owned(),
        ),
        allow_passthrough_support: crate::diagnostics::TmuxSupportFact::Supported,
        allow_passthrough: crate::diagnostics::TmuxOptionFact::Available(
            if evidence == TmuxEvidence::DcsPassthrough {
                "off"
            } else {
                "on"
            }
            .to_owned(),
        ),
        color_passthrough: if evidence == TmuxEvidence::ColorPassthrough {
            crate::diagnostics::TmuxColorPassthrough::Reduced
        } else {
            crate::diagnostics::TmuxColorPassthrough::Forwarded
        },
    };
    report.findings.push(DiagnosticFinding {
        id,
        disposition: FindingDisposition::Issue,
        message: "tmux option disabled".to_owned(),
        remediation: None,
        automatic_remediation: automatic_remediation_for(id),
        note: None,
    });
    report
}

fn tmux_request(home: &Path, id: DiagnosticId) -> FixRequest {
    FixRequest::new_for_test(id, home, None, None, None).unwrap()
}

#[test]
fn tmux_fix_registry_resolves_every_short_and_canonical_id() {
    for (id, handle, _) in automatic_fix_choices() {
        assert_eq!(resolve_fix_id(handle).unwrap(), id);
        assert_eq!(resolve_fix_id(&id.to_string()).unwrap(), id);
        assert_eq!(
            human_fix_command(id).unwrap(),
            format!("echoagent doctor fix {handle}")
        );
    }
}

#[test]
fn tmux_fix_is_available_here_in_remote_sessions_while_ssh_wrap_stays_local_only() {
    let temp = tempfile::tempdir().unwrap();
    let mut terminal = tmux_terminal(false);
    terminal.is_ssh = true;
    let mut report = tmux_report(TMUX_CLIPBOARD_ID, TmuxEvidence::Clipboard);
    report.facts.ssh = true;
    assert_eq!(
        applicable_automatic_fixes_with(&report, &terminal, |id| {
            Ok(tmux_request(temp.path(), id))
        }),
        vec![(
            TMUX_CLIPBOARD_ID,
            "tmux-clipboard",
            AutomaticFixAvailability::Here,
        )]
    );
    assert!(
        plan_fix(
            tmux_request(temp.path(), TMUX_CLIPBOARD_ID),
            &report,
            &terminal,
        )
        .is_ok()
    );
}

#[test]
fn tmux_specs_plan_exact_independent_managed_items() {
    let temp = tempfile::tempdir().unwrap();
    for (id, evidence, line) in [
        (
            TMUX_CLIPBOARD_ID,
            TmuxEvidence::Clipboard,
            "set -g set-clipboard on",
        ),
        (
            DCS_PASSTHROUGH_ID,
            TmuxEvidence::DcsPassthrough,
            "set -wg allow-passthrough on",
        ),
        (
            TMUX_EXTENDED_KEYS_ID,
            TmuxEvidence::ExtendedKeys,
            "set -g extended-keys on",
        ),
        (
            TMUX_TRUECOLOR_ID,
            TmuxEvidence::ColorPassthrough,
            "set -as terminal-features \",*:RGB\"",
        ),
    ] {
        let plan = plan_fix(
            tmux_request(temp.path(), id),
            &tmux_report(id, evidence),
            &tmux_terminal(false),
        )
        .unwrap();
        assert_eq!(plan.change().requested_path, temp.path().join(".tmux.conf"));
        assert!(
            plan.change()
                .block
                .contains(&format!("# >>> {id} >>>\n{line}\n# <<< {id} <<<"))
        );
        assert!(!plan.change().block.contains("terminal.ssh-wrap"));
        let preview = format_fix_preview(&plan);
        assert!(preview.contains("does not reload or modify the live tmux server"));
        assert!(preview.contains("Run /doctor again to verify the live setting"));
    }
}

#[test]
fn safe_absolute_directory_rejects_hostile_home_and_byobu_values() {
    for value in [
        ".",
        "..",
        "/",
        "relative",
        "/tmp/../escape",
        "/tmp/bad\nname",
        "~/x",
    ] {
        assert!(
            matches!(
                SafeAbsoluteDirectory::parse(PathBuf::from(value), "HOME"),
                Err(FixError::UnsafeDirectory { .. })
            ),
            "{value:?}"
        );
    }
}

#[test]
fn reload_instruction_shell_quotes_and_markdown_escapes_paths() {
    assert_eq!(
        reload_instruction(Path::new("/tmp/a b/q'v.conf")),
        "Reload tmux with `tmux source-file '/tmp/a b/q'\\''v.conf'`, or restart the tmux server."
    );
    assert_eq!(
        reload_instruction(Path::new("/tmp/a`b.conf")),
        "Reload tmux with ``tmux source-file '/tmp/a`b.conf'``, or restart the tmux server."
    );
    assert_eq!(
        shell_quote_path(Path::new("/tmp/a`b.conf")).unwrap(),
        "'/tmp/a`b.conf'"
    );
    assert_eq!(
        reload_instruction(Path::new("/tmp/bad\npath")),
        "Reload your tmux config, or restart the tmux server, to activate the persistent setting."
    );
    assert_eq!(markdown_code_path(Path::new("/tmp/a`b")), "``/tmp/a`b``");
}

#[cfg(unix)]
#[test]
fn full_preview_safely_renders_backtick_requested_symlink_target_and_backup_paths() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().unwrap();
    let root = dunce::canonicalize(temp.path()).unwrap();
    let home = root.join("home`dir");
    let target_dir = root.join("target`dir");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&target_dir).unwrap();
    let target = target_dir.join("tmux`target.conf");
    std::fs::write(&target, "set -g mouse on\n").unwrap();
    symlink(&target, home.join(".tmux.conf")).unwrap();
    let plan = plan_fix(
        tmux_request(&home, TMUX_CLIPBOARD_ID),
        &tmux_report(TMUX_CLIPBOARD_ID, TmuxEvidence::Clipboard),
        &tmux_terminal(false),
    )
    .unwrap();
    let preview = format_fix_preview(&plan);
    assert!(preview.contains("File: ``"), "{preview}");
    assert!(preview.contains("Actual file: ``"), "{preview}");
    assert!(preview.contains("Backup will be saved to: ``"), "{preview}");
    assert!(preview.contains("home`dir/.tmux.conf"), "{preview}");
    assert!(preview.contains("tmux`target.conf"), "{preview}");
}

#[test]
fn tmux_plain_byobu_and_custom_config_paths_are_physical() {
    let temp = tempfile::tempdir().unwrap();
    let report = tmux_report(TMUX_CLIPBOARD_ID, TmuxEvidence::Clipboard);
    let plain = plan_fix(
        tmux_request(temp.path(), TMUX_CLIPBOARD_ID),
        &report,
        &tmux_terminal(false),
    )
    .unwrap();
    assert_eq!(
        plain.change().requested_path,
        temp.path().join(".tmux.conf")
    );
    assert!(
        !plain
            .change()
            .requested_path
            .to_string_lossy()
            .contains('~')
    );

    let custom = FixRequest::new_for_test(
        TMUX_CLIPBOARD_ID,
        temp.path(),
        None,
        None,
        Some(temp.path().join("custom-byobu")),
    )
    .unwrap();
    let byobu = plan_fix(custom, &report, &tmux_terminal(true)).unwrap();
    assert_eq!(
        byobu.change().requested_path,
        temp.path().join("custom-byobu/.tmux.conf")
    );

    assert!(matches!(
        plan_fix(
            tmux_request(temp.path(), TMUX_CLIPBOARD_ID),
            &report,
            &tmux_terminal(true)
        ),
        Err(FixError::ByobuConfigUnavailable)
    ));
}

#[test]
fn tmux_managed_items_coexist_and_each_apply_is_one_transaction() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join(".tmux.conf");
    for (id, evidence, line) in [
        (
            TMUX_CLIPBOARD_ID,
            TmuxEvidence::Clipboard,
            "set -g set-clipboard on",
        ),
        (
            DCS_PASSTHROUGH_ID,
            TmuxEvidence::DcsPassthrough,
            "set -wg allow-passthrough on",
        ),
        (
            TMUX_EXTENDED_KEYS_ID,
            TmuxEvidence::ExtendedKeys,
            "set -g extended-keys on",
        ),
        (
            TMUX_TRUECOLOR_ID,
            TmuxEvidence::ColorPassthrough,
            "set -as terminal-features \",*:RGB\"",
        ),
    ] {
        let plan = plan_fix(
            tmux_request(temp.path(), id),
            &tmux_report(id, evidence),
            &tmux_terminal(false),
        )
        .unwrap();
        let outcome = apply_fix(plan).unwrap();
        assert_eq!(outcome.activation(), FixActivation::RequiresReload);
        assert_eq!(outcome.changed_path(), path);
        assert!(format_fix_success(&outcome).contains("Run /doctor again"));
        assert!(std::fs::read_to_string(&path).unwrap().contains(line));
    }
    let content = std::fs::read_to_string(&path).unwrap();
    assert_eq!(content.matches("# >>> echoagent doctor >>>").count(), 1);
    for id in [
        TMUX_CLIPBOARD_ID,
        DCS_PASSTHROUGH_ID,
        TMUX_EXTENDED_KEYS_ID,
        TMUX_TRUECOLOR_ID,
    ] {
        assert_eq!(content.matches(&format!("# >>> {id} >>>")).count(), 1);
    }
}

#[test]
fn tmux_scanner_handles_server_scopes_separators_prefixes_and_native_blocks() {
    let path = Path::new("/tmp/tmux.conf");
    for spec in [&TMUX_CLIPBOARD_SPEC, &TMUX_EXTENDED_KEYS_SPEC] {
        let healthy = spec.healthy_values[0];
        for assignment in [
            format!("set {} {healthy}\n", spec.option),
            format!("set -s {} {healthy}\n", spec.option),
            format!("set-option -gq {} {healthy}\n", spec.option),
            format!("set -w {} {healthy}\n", spec.option),
            format!("FOO=bar set -g {} {healthy}\n", spec.option),
            format!("set -g mouse on; set -g {} {healthy}\n", spec.option),
        ] {
            assert_eq!(
                scan_direct_tmux_option(&assignment, path, spec).unwrap(),
                DirectOptionState::Healthy,
                "{assignment:?}"
            );
        }
        for conflict in [
            format!("set {} off\n", spec.option),
            format!("set -s {} off\n", spec.option),
            format!("set-option -g {} off\n", spec.option),
            format!("set -w {} off\n", spec.option),
            format!("set -g mouse on; set -g {} off\n", spec.option),
            format!("set -g {} o\\\nff\n", spec.option),
        ] {
            assert!(
                matches!(
                    scan_direct_tmux_option(&conflict, path, spec),
                    Err(FixError::ExistingCustomization { .. })
                ),
                "{conflict:?}"
            );
        }
    }

    let spec = &DCS_PASSTHROUGH_SPEC;
    for healthy in [
        "setw -g allow-passthrough on\n",
        "set-window-option -g allow-passthrough all\n",
        "set -wg allow-passthrough on\n",
    ] {
        assert_eq!(
            scan_direct_tmux_option(healthy, path, spec).unwrap(),
            DirectOptionState::Healthy,
            "{healthy:?}"
        );
    }
    for conflict in [
        "setw -g allow-passthrough off\n",
        "set-window-option -g allow-passthrough off\n",
        "set -wg allow-passthrough off\n",
    ] {
        assert!(
            matches!(
                scan_direct_tmux_option(conflict, path, spec),
                Err(FixError::ExistingCustomization { .. })
            ),
            "{conflict:?}"
        );
    }
    for local in [
        "set allow-passthrough on\n",
        "setw allow-passthrough on\n",
        "setw -t:1 allow-passthrough off\n",
    ] {
        assert_eq!(
            scan_direct_tmux_option(local, path, spec).unwrap(),
            DirectOptionState::Absent,
            "{local:?}"
        );
    }

    for spec in [
        &TMUX_CLIPBOARD_SPEC,
        &DCS_PASSTHROUGH_SPEC,
        &TMUX_EXTENDED_KEYS_SPEC,
    ] {
        for ignored in [
            format!("# set -g {} off\n", spec.option),
            format!("set -g @{} off\n", spec.option),
            format!("set -g {}-copy off\n", spec.option),
            format!("%if 1\nset -g {} off\n%endif\n", spec.option),
            format!("if-shell true {{ set -g {} off }}\n", spec.option),
        ] {
            assert_eq!(
                scan_direct_tmux_option(&ignored, path, spec).unwrap(),
                DirectOptionState::Absent,
                "{ignored:?}"
            );
        }
        for ambiguous in [
            format!("se -g {} off\n", spec.option),
            format!("set -g {} off extra\n", spec.option),
            format!("set -g {}\n", spec.option),
            format!("set -g {} 'unterminated\n", spec.option),
            format!("set -g {} \\\n", spec.option),
            format!("set -t target\nset -g {} off\n", spec.option),
        ] {
            assert!(
                matches!(
                    scan_direct_tmux_option(&ambiguous, path, spec),
                    Err(FixError::ExistingCustomization { .. })
                ),
                "{ambiguous:?}"
            );
        }
    }
}

#[test]
fn conflicting_direct_form_after_managed_block_fails_persistent_verification() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join(".tmux.conf");
    for conflict in [
        "set set-clipboard off",
        "set -s set-clipboard off",
        "set-option -g set-clipboard off",
        "set -g mouse on; set -g set-clipboard off",
        "se -g set-clipboard off",
    ] {
        std::fs::write(
            &path,
            format!(
                "# >>> echoagent doctor >>>\n# >>> terminal.tmux-clipboard >>>\nset -g set-clipboard on\n# <<< terminal.tmux-clipboard <<<\n# <<< echoagent doctor <<<\n{conflict}\n"
            ),
        )
        .unwrap();
        assert!(
            !tmux_option_configured(&path, &TMUX_CLIPBOARD_SPEC),
            "{conflict}"
        );
    }
}

#[test]
fn healthy_direct_does_not_suppress_repair_of_noncanonical_managed_item() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join(".tmux.conf");
    let report = tmux_report(TMUX_CLIPBOARD_ID, TmuxEvidence::Clipboard);
    for content in [
        "set -g set-clipboard on\n# >>> echoagent doctor >>>\n# >>> terminal.tmux-clipboard >>>\nset -g set-clipboard off\n# <<< terminal.tmux-clipboard <<<\n# <<< echoagent doctor <<<\n",
        "# >>> echoagent doctor >>>\n# >>> terminal.tmux-clipboard >>>\nset -g set-clipboard off\n# <<< terminal.tmux-clipboard <<<\n# <<< echoagent doctor <<<\nset -g set-clipboard on\n",
    ] {
        std::fs::write(&path, content).unwrap();
        let plan = plan_fix(
            tmux_request(temp.path(), TMUX_CLIPBOARD_ID),
            &report,
            &tmux_terminal(false),
        )
        .unwrap();
        assert!(format_fix_preview(&plan).contains("Text to add:\n"));
        let outcome = apply_fix(plan).unwrap();
        assert_eq!(outcome.status(), FixStatus::Applied);
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains("# >>> terminal.tmux-clipboard >>>\nset -g set-clipboard on\n")
        );
    }
}

#[test]
fn tmux_applicability_uses_exact_positive_probe_gates() {
    let temp = tempfile::tempdir().unwrap();
    let terminal = tmux_terminal(false);
    let mut clipboard = tmux_report(TMUX_CLIPBOARD_ID, TmuxEvidence::Clipboard);
    clipboard.facts.tmux.set_clipboard =
        crate::diagnostics::TmuxOptionFact::Available("external".to_owned());
    assert!(matches!(
        plan_fix(
            tmux_request(temp.path(), TMUX_CLIPBOARD_ID),
            &clipboard,
            &terminal
        ),
        Err(FixError::TmuxNotApplicable)
    ));

    let mut dcs = tmux_report(DCS_PASSTHROUGH_ID, TmuxEvidence::DcsPassthrough);
    for support in [
        crate::diagnostics::TmuxSupportFact::Unsupported,
        crate::diagnostics::TmuxSupportFact::Unavailable,
        crate::diagnostics::TmuxSupportFact::Error,
    ] {
        dcs.facts.tmux.allow_passthrough_support = support;
        assert!(matches!(
            plan_fix(
                tmux_request(temp.path(), DCS_PASSTHROUGH_ID),
                &dcs,
                &terminal
            ),
            Err(FixError::TmuxNotApplicable)
        ));
    }

    let mut extended = tmux_report(TMUX_EXTENDED_KEYS_ID, TmuxEvidence::ExtendedKeys);
    extended.facts.tmux.extended_keys = crate::diagnostics::TmuxOptionFact::Unavailable;
    assert!(matches!(
        plan_fix(
            tmux_request(temp.path(), TMUX_EXTENDED_KEYS_ID),
            &extended,
            &terminal
        ),
        Err(FixError::TmuxNotApplicable)
    ));
}

#[test]
fn tmux_stale_plan_and_idempotence_reuse_managed_writer_safety() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join(".tmux.conf");
    std::fs::write(&path, "set -g mouse on\n").unwrap();
    let report = tmux_report(TMUX_CLIPBOARD_ID, TmuxEvidence::Clipboard);
    let plan = plan_fix(
        tmux_request(temp.path(), TMUX_CLIPBOARD_ID),
        &report,
        &tmux_terminal(false),
    )
    .unwrap();
    std::fs::write(&path, "set -g mouse off\n").unwrap();
    assert!(matches!(
        apply_fix(plan),
        Err(FixError::TmuxManaged(
            echo_agent_config::managed_text::ManagedConfigError::StalePlan(_)
        ))
    ));

    std::fs::write(&path, "set -g set-clipboard on\n").unwrap();
    let plan = plan_fix(
        tmux_request(temp.path(), TMUX_CLIPBOARD_ID),
        &report,
        &tmux_terminal(false),
    )
    .unwrap();
    let preview = format_fix_preview(&plan);
    assert!(preview.contains("Text to add: None"), "{preview}");
    assert!(!preview.contains("Backup will be saved"), "{preview}");
    let outcome = apply_fix(plan).unwrap();
    assert_eq!(outcome.status(), FixStatus::AlreadyConfigured);
    assert!(verify_persistent_fix(&outcome));
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "set -g set-clipboard on\n"
    );

    let stale = plan_fix(
        tmux_request(temp.path(), TMUX_CLIPBOARD_ID),
        &report,
        &tmux_terminal(false),
    )
    .unwrap();
    std::fs::write(&path, "set -g set-clipboard off\n").unwrap();
    assert!(matches!(
        apply_fix(stale),
        Err(FixError::TmuxManaged(
            echo_agent_config::managed_text::ManagedConfigError::StalePlan(_)
        ))
    ));
}

#[test]
fn bash_zsh_and_fish_plans_use_exact_paths_and_aliases() {
    let temp = tempfile::tempdir().unwrap();
    for (shell, relative, alias) in [
        ("/bin/bash", ".bashrc", "alias ssh='echoagent wrap ssh'"),
        ("/bin/zsh", ".zshrc", "alias ssh='echoagent wrap ssh'"),
        (
            "/usr/local/bin/fish",
            ".config/fish/config.fish",
            "alias ssh 'echoagent wrap ssh'",
        ),
    ] {
        let plan = plan_fix(request(temp.path(), shell), &report(), &terminal()).unwrap();
        assert_eq!(plan.id(), SSH_WRAP_ID);
        assert_eq!(plan.change().requested_path, temp.path().join(relative));
        assert_eq!(
            plan.change().block,
            format!(
                "# >>> echoagent doctor >>>\n# >>> terminal.ssh-wrap >>>\n{alias}\n# <<< terminal.ssh-wrap <<<\n# <<< echoagent doctor <<<"
            )
        );
        assert!(
            plan.caveats()
                .iter()
                .any(|line| line.contains("command ssh"))
        );
        assert!(plan.caveats().iter().any(|line| line.contains("ssh -f")));
        assert!(
            plan.caveats()
                .iter()
                .any(|line| line.contains("ControlPersist"))
        );
        assert!(plan.caveats().iter().any(|line| line.contains("~^Z")));
    }
}

#[test]
fn remote_vscode_and_unsupported_shell_are_refused() {
    let temp = tempfile::tempdir().unwrap();
    let mut remote = terminal();
    remote.is_ssh = true;
    assert!(matches!(
        plan_fix(request(temp.path(), "/bin/zsh"), &report(), &remote),
        Err(FixError::RemoteSession)
    ));

    let mut vscode = terminal();
    vscode.is_official_vscode_remote = true;
    assert!(matches!(
        plan_fix(request(temp.path(), "/bin/zsh"), &report(), &vscode),
        Err(FixError::NotApplicable)
    ));
    assert!(matches!(
        plan_fix(request(temp.path(), "/bin/tcsh"), &report(), &terminal()),
        Err(FixError::UnsupportedShell)
    ));
}

#[cfg(windows)]
#[test]
fn windows_is_manual_only_before_shell_selection() {
    let temp = tempfile::tempdir().unwrap();
    let mut request = request(temp.path(), "C:\\Program Files\\Git\\bin\\bash.exe");
    request.shell = Some(PathBuf::from("bash"));
    assert!(matches!(
        plan_fix(request, &report(), &terminal()),
        Err(FixError::PlatformUnsupported)
    ));
}

#[test]
fn existing_alias_and_function_conflicts_are_preserved() {
    let cases = [
        ("/bin/bash", ".bashrc", "alias ssh='ssh -A'\n"),
        ("/bin/zsh", ".zshrc", "ssh() { command ssh -A \"$@\"; }\n"),
        (
            "/usr/bin/fish",
            ".config/fish/config.fish",
            "function ssh\n  command ssh -A $argv\nend\n",
        ),
    ];
    for (shell, relative, content) in cases {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, content).unwrap();
        assert!(matches!(
            plan_fix(request(temp.path(), shell), &report(), &terminal()),
            Err(FixError::ExistingCustomization { .. })
        ));
        assert_eq!(std::fs::read_to_string(path).unwrap(), content);
    }
}

#[test]
fn alias_and_fish_function_scanners_accept_shell_whitespace() {
    for declaration in [
        "alias  ssh='ssh -A'",
        "alias\tssh = 'ssh -A'",
        "alias \t ssh='ssh -A'",
    ] {
        assert!(
            detect_posix_ssh_customization(declaration).is_some(),
            "{declaration}"
        );
    }
    for declaration in [
        "alias  ssh 'ssh -A'",
        "alias\tssh='ssh -A'",
        "function  ssh",
        "function\tssh --description wrapped",
    ] {
        assert!(
            detect_fish_ssh_customization(declaration).is_some(),
            "{declaration}"
        );
    }
    for not_ssh in [
        "aliases ssh='ssh -A'",
        "alias ssh_wrap='ssh -A'",
        "alias sshuttle='ssh -A'",
    ] {
        assert!(
            detect_posix_ssh_customization(not_ssh).is_none(),
            "{not_ssh}"
        );
        assert!(
            detect_fish_ssh_customization(not_ssh).is_none(),
            "{not_ssh}"
        );
    }
}

#[test]
fn posix_function_scanner_requires_exact_ssh_name_boundary() {
    for declaration in [
        "function ssh { command ssh \"$@\"; }",
        "function ssh() { command ssh \"$@\"; }",
        "ssh() { command ssh \"$@\"; }",
        "ssh () { command ssh \"$@\"; }",
    ] {
        assert!(
            detect_posix_ssh_customization(declaration).is_some(),
            "{declaration}"
        );
    }
    for not_ssh in [
        "function ssh_wrap { :; }",
        "function sshuttle { :; }",
        "ssh_wrap() { :; }",
        "sshuttle () { :; }",
    ] {
        assert!(
            detect_posix_ssh_customization(not_ssh).is_none(),
            "{not_ssh}"
        );
    }
}

#[test]
fn conflict_scan_uses_the_exact_validated_source_snapshot() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join(".bashrc");
    std::fs::write(&path, "export KEEP=1\n").unwrap();
    let plan = plan_fix(request(temp.path(), "/bin/bash"), &report(), &terminal()).unwrap();
    std::fs::write(&path, "alias ssh='ssh -A'\n").unwrap();
    assert!(matches!(
        apply_fix(plan),
        Err(FixError::Managed(
            echo_agent_config::managed_text::ManagedConfigError::StalePlan(_)
        ))
    ));
    assert_eq!(
        std::fs::read_to_string(path).unwrap(),
        "alias ssh='ssh -A'\n"
    );
}

#[test]
fn non_utf8_source_fails_closed_before_conflict_policy() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join(".zshrc");
    std::fs::write(&path, [0xff]).unwrap();
    assert!(matches!(
        plan_fix(request(temp.path(), "/bin/zsh"), &report(), &terminal()),
        Err(FixError::Managed(
            echo_agent_config::managed_text::ManagedConfigError::UnsafePath { .. }
        ))
    ));
}

#[cfg(unix)]
#[test]
fn validator_prefers_custom_executable_shell_and_uses_path_for_basename_only() {
    use std::os::unix::fs::PermissionsExt as _;

    let temp = tempfile::tempdir().unwrap();
    let shadow = temp.path().join("shadow");
    let valid = temp.path().join("valid");
    std::fs::create_dir(&shadow).unwrap();
    std::fs::create_dir(&valid).unwrap();
    std::fs::write(shadow.join("bash"), "not executable").unwrap();
    let real = valid.join("bash");
    std::fs::write(&real, "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(
        find_on_path_in("bash", [&shadow, &valid]),
        Some(real.clone())
    );

    let custom = temp.path().join("custom/bash");
    std::fs::create_dir_all(custom.parent().unwrap()).unwrap();
    std::fs::write(&custom, "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&custom, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(resolve_validator_program(&custom), Some(custom.clone()));

    std::fs::set_permissions(&custom, std::fs::Permissions::from_mode(0o644)).unwrap();
    // A non-executable explicit SHELL path is not silently substituted with a
    // different same-basename shell from PATH.
    assert_eq!(resolve_validator_program(&custom), None);

    assert_eq!(
        find_on_path_in("bash", [&shadow, &valid]),
        Some(real),
        "basename-only shell names may resolve through PATH"
    );
}

#[test]
fn comments_and_managed_alias_do_not_create_false_conflicts() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join(".zshrc");
    std::fs::write(
        &path,
        "# alias ssh='ssh -A'\n# >>> echoagent doctor >>>\n# >>> terminal.ssh-wrap >>>\nalias ssh='echoagent wrap ssh'\n# <<< terminal.ssh-wrap <<<\n# <<< echoagent doctor <<<\n",
    )
    .unwrap();
    let plan = plan_fix(request(temp.path(), "/bin/zsh"), &report(), &terminal()).unwrap();
    let outcome = apply_fix(plan).unwrap();
    assert_eq!(outcome.status(), FixStatus::AlreadyConfigured);
    assert!(outcome.backup_path().is_none());
}

#[test]
fn managed_alias_with_later_unmanaged_conflict_is_not_configured() {
    let cases = [
        (
            ShellKind::Bash,
            "# >>> echoagent doctor >>>\n# >>> terminal.ssh-wrap >>>\nalias ssh='echoagent wrap ssh'\n# <<< terminal.ssh-wrap <<<\n# <<< echoagent doctor <<<\nalias ssh='ssh -A'\n",
        ),
        (
            ShellKind::Fish,
            "# >>> echoagent doctor >>>\n# >>> terminal.ssh-wrap >>>\nalias ssh 'echoagent wrap ssh'\n# <<< terminal.ssh-wrap <<<\n# <<< echoagent doctor <<<\nfunction ssh\n  command ssh -A $argv\nend\n",
        ),
    ];
    for (shell, content) in cases {
        let temp = tempfile::tempdir().unwrap();
        let path = shell.config_path(temp.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, content).unwrap();
        assert!(!managed_alias_configured(&path, shell));
    }
}

#[test]
fn stale_plan_is_rejected_and_apply_verifies_postcondition() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join(".bashrc");
    std::fs::write(&path, "export KEEP=1\n").unwrap();
    let plan = plan_fix(request(temp.path(), "/bin/bash"), &report(), &terminal()).unwrap();
    std::fs::write(&path, "export KEEP=2\n").unwrap();
    assert!(matches!(
        apply_fix(plan),
        Err(FixError::Managed(
            echo_agent_config::managed_text::ManagedConfigError::StalePlan(_)
        ))
    ));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "export KEEP=2\n");

    let plan = plan_fix(request(temp.path(), "/bin/bash"), &report(), &terminal()).unwrap();
    let outcome = apply_fix(plan).unwrap();
    assert_eq!(outcome.status(), FixStatus::Applied);
    assert_eq!(outcome.id(), SSH_WRAP_ID);
    assert_eq!(outcome.shell(), Some(ShellKind::Bash));
    assert!(managed_alias_configured(&path, ShellKind::Bash));
    assert!(outcome.managed_alias_is_configured());
}

#[test]
fn ssh_wrap_outcome_verifies_with_planned_shell_not_process_shell() {
    // Post-apply verification must use the shell stored on the outcome. Even if
    // `$SHELL` is missing or points at a different shell family, a successful
    // apply against bash must still report the managed alias as configured.
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join(".bashrc");
    let plan = plan_fix(request(temp.path(), "/bin/bash"), &report(), &terminal()).unwrap();
    let outcome = apply_fix(plan).unwrap();
    assert_eq!(outcome.shell(), Some(ShellKind::Bash));
    assert_eq!(outcome.changed_path(), path);
    assert!(outcome.managed_alias_is_configured());

    // Fish uses a different alias syntax; checking the bash-written path with
    // fish must not count as configured. The outcome keeps bash regardless.
    assert!(!managed_alias_configured(&path, ShellKind::Fish));
    assert!(
        outcome.managed_alias_is_configured(),
        "outcome must keep the planned bash shell, not re-derive from $SHELL"
    );

    let filtered = configured_report(report(), outcome.managed_alias_is_configured());
    assert!(
        !filtered
            .findings
            .iter()
            .any(|finding| finding.id == SSH_WRAP_ID),
        "configured_report must drop ssh-wrap when outcome shell matches the write"
    );
}

#[test]
fn configured_report_reaches_pass_state_only_for_exact_managed_alias() {
    let mut diagnostic = report();
    diagnostic = configured_report(diagnostic, false);
    assert!(
        diagnostic
            .findings
            .iter()
            .any(|finding| finding.id == SSH_WRAP_ID)
    );
    diagnostic = configured_report(diagnostic, true);
    assert!(
        !diagnostic
            .findings
            .iter()
            .any(|finding| finding.id == SSH_WRAP_ID)
    );

    let temp = tempfile::tempdir().unwrap();
    let mut healthy = report();
    healthy.findings.clear();
    let plan = plan_fix(request(temp.path(), "/bin/bash"), &healthy, &terminal()).unwrap();
    assert_eq!(
        plan.id(),
        SSH_WRAP_ID,
        "healthy reports can plan idempotent setup"
    );
}

#[cfg(unix)]
#[test]
fn shell_aliases_expand_to_exact_argv_and_bypass_is_explicit() {
    let temp = tempfile::tempdir().unwrap();
    let capture = temp.path().join("capture");
    let echoagent = temp.path().join("echoagent");
    std::fs::write(
        &echoagent,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\n",
            capture.display()
        ),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(&echoagent, std::fs::Permissions::from_mode(0o755)).unwrap();

    if let Some(bash) = find_on_path("bash") {
        let rc = temp.path().join("bashrc");
        std::fs::write(&rc, "alias ssh='echoagent wrap ssh'\n").unwrap();
        let command = format!(
            "source '{}'; source '{}'; eval 'ssh -p 2222 host'",
            rc.display(),
            rc.display()
        );
        let mut shell = std::process::Command::new(bash);
        shell
            .args(["-ic", &command])
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    temp.path().display(),
                    std::env::var("PATH").unwrap()
                ),
            )
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .envs(echo_agent_tty_utils::pager_env());
        echo_agent_tty_utils::detach_std_command(&mut shell);
        let status = shell.status().unwrap();
        assert!(status.success());
        assert_eq!(
            std::fs::read_to_string(&capture).unwrap(),
            "wrap\nssh\n-p\n2222\nhost\n"
        );
    }
    if let Some(zsh) = find_on_path("zsh") {
        let rc = temp.path().join("zshrc");
        std::fs::write(&rc, "alias ssh='echoagent wrap ssh'\n").unwrap();
        let command = format!(
            "source '{}'; source '{}'; eval 'ssh -p 2222 host'",
            rc.display(),
            rc.display()
        );
        let mut shell = std::process::Command::new(zsh);
        shell
            .args(["-dfc", &command])
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    temp.path().display(),
                    std::env::var("PATH").unwrap()
                ),
            )
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .envs(echo_agent_tty_utils::pager_env());
        echo_agent_tty_utils::detach_std_command(&mut shell);
        let status = shell.status().unwrap();
        assert!(status.success());
        assert_eq!(
            std::fs::read_to_string(&capture).unwrap(),
            "wrap\nssh\n-p\n2222\nhost\n"
        );
    }

    let fake_bin = temp.path().join("fake-bin");
    std::fs::create_dir(&fake_bin).unwrap();
    let fake_ssh = fake_bin.join("ssh");
    std::fs::write(&fake_ssh, "#!/bin/sh\nprintf bypass > \"$CAPTURE\"\n").unwrap();
    std::fs::set_permissions(&fake_ssh, std::fs::Permissions::from_mode(0o755)).unwrap();
    let Some(bash) = find_on_path("bash") else {
        return;
    };
    let mut shell = std::process::Command::new(bash);
    shell
        .args(["-ic", "alias ssh='echoagent wrap ssh'; command ssh host"])
        .env("CAPTURE", &capture)
        .env(
            "PATH",
            format!(
                "{}:{}:{}",
                fake_bin.display(),
                temp.path().display(),
                std::env::var("PATH").unwrap()
            ),
        )
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .envs(echo_agent_tty_utils::pager_env());
    echo_agent_tty_utils::detach_std_command(&mut shell);
    let status = shell.status().unwrap();
    assert!(status.success());
    assert_eq!(std::fs::read_to_string(&capture).unwrap(), "bypass");

    if let Some(fish) = find_on_path("fish") {
        let fish_capture = temp.path().join("fish-capture");
        let fish_echo_agent = temp.path().join("fish-echo-agent");
        std::fs::write(
            &fish_echo_agent,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\n",
                fish_capture.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&fish_echo_agent, std::fs::Permissions::from_mode(0o755)).unwrap();
        let rc = temp.path().join("config.fish");
        std::fs::write(&rc, "alias ssh 'fish-echo-agent wrap ssh'\n").unwrap();
        let command = format!(
            "source '{}'; source '{}'; ssh -p 2222 host; env | string match -rq '^ssh='; and exit 9; or exit 0",
            rc.display(),
            rc.display()
        );
        let mut shell = std::process::Command::new(fish);
        shell
            .args(["-c", &command])
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    temp.path().display(),
                    std::env::var("PATH").unwrap()
                ),
            )
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .envs(echo_agent_tty_utils::pager_env());
        echo_agent_tty_utils::detach_std_command(&mut shell);
        assert!(shell.status().unwrap().success());
        assert_eq!(
            std::fs::read_to_string(fish_capture).unwrap(),
            "wrap\nssh\n-p\n2222\nhost\n"
        );
    } else {
        eprintln!("fish unavailable; fish runtime alias test skipped explicitly");
    }
}

/// An accumulating remedy is additive, so a user's own `terminal-features`
/// lines are not a conflict: tmux applies EchoAgent's managed block last and the
/// features merge. A direct-assignment remedy would refuse to touch the file.
#[test]
fn tmux_truecolor_fix_appends_alongside_existing_terminal_features() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join(".tmux.conf");
    std::fs::write(
        &path,
        "set -g mouse on\nset -as terminal-features \",xterm-256color:RGB\"\n",
    )
    .unwrap();

    let plan = plan_fix(
        tmux_request(temp.path(), TMUX_TRUECOLOR_ID),
        &tmux_report(TMUX_TRUECOLOR_ID, TmuxEvidence::ColorPassthrough),
        &tmux_terminal(false),
    )
    .unwrap();
    let outcome = apply_fix(plan).unwrap();

    assert_eq!(outcome.status(), FixStatus::Applied);
    let content = std::fs::read_to_string(&path).unwrap();
    assert!(content.contains("set -as terminal-features \",xterm-256color:RGB\""));
    assert!(content.contains("set -as terminal-features \",*:RGB\""));
}

#[test]
fn tmux_truecolor_fix_requires_a_reducing_client() {
    let temp = tempfile::tempdir().unwrap();
    let report = tmux_report(TMUX_TRUECOLOR_ID, TmuxEvidence::Clipboard);

    assert!(matches!(
        plan_fix(
            tmux_request(temp.path(), TMUX_TRUECOLOR_ID),
            &report,
            &tmux_terminal(false)
        ),
        Err(FixError::TmuxNotApplicable)
    ));
}
