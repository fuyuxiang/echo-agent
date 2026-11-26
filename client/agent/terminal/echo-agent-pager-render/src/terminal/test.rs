use super::*;

// -- terminal_name_from_term_program (existing coverage) ------------------

#[test]
fn test_terminal_name_from_term_program() {
    assert_eq!(
        terminal_name_from_term_program("iTerm.app"),
        Some(TerminalName::Iterm2)
    );
    assert_eq!(
        terminal_name_from_term_program("WarpTerminal"),
        Some(TerminalName::WarpTerminal)
    );
    assert_eq!(
        terminal_name_from_term_program("Ghostty"),
        Some(TerminalName::Ghostty)
    );
    assert_eq!(
        terminal_name_from_term_program("Apple_Terminal"),
        Some(TerminalName::AppleTerminal)
    );
    assert_eq!(
        terminal_name_from_term_program("zed"),
        Some(TerminalName::Zed)
    );
    assert_eq!(
        terminal_name_from_term_program("WindowsTerminal"),
        Some(TerminalName::WindowsTerminal)
    );
    assert_eq!(
        terminal_name_from_term_program("rio"),
        Some(TerminalName::Rio)
    );
    assert_eq!(
        terminal_name_from_term_program("otty"),
        Some(TerminalName::Otty)
    );
    assert_eq!(
        terminal_name_from_term_program("Otty"),
        Some(TerminalName::Otty)
    );
    assert_eq!(terminal_name_from_term_program("unknown-term"), None);
}

#[test]
fn brand_otty_from_term_program() {
    let env = env_from(&[("TERM_PROGRAM", "otty")]);
    assert_eq!(detect_terminal_brand_from_env(&env), TerminalName::Otty);
    let env = env_from(&[("TERM_PROGRAM", "Otty")]);
    assert_eq!(detect_terminal_brand_from_env(&env), TerminalName::Otty);
}

#[test]
fn otty_delivers_ime_as_bracketed_paste_only() {
    assert!(TerminalName::Otty.delivers_ime_as_bracketed_paste());
    for brand in [
        TerminalName::AppleTerminal,
        TerminalName::Ghostty,
        TerminalName::Iterm2,
        TerminalName::Unknown,
        TerminalName::WezTerm,
        TerminalName::Kitty,
    ] {
        assert!(
            !brand.delivers_ime_as_bracketed_paste(),
            "{brand:?} must not gate IME bracketed-paste origin"
        );
    }
}

#[test]
fn otty_is_capability_unclassified_like_unknown() {
    assert!(TerminalName::Otty.is_capability_unclassified());
    assert!(TerminalName::Unknown.is_capability_unclassified());
    assert!(!TerminalName::Ghostty.is_capability_unclassified());
    assert!(!TerminalName::AppleTerminal.is_capability_unclassified());
}

#[test]
fn otty_skips_kitty_keyboard_like_unknown() {
    let ctx = TerminalContext {
        brand: TerminalName::Otty,
        env_brand: TerminalName::Otty,
        multiplexer: MultiplexerKind::Undetected,
        ..Default::default()
    };
    assert_eq!(ctx.kitty_skip_reason(), Some("unknown_no_multiplexer"));
    assert!(ctx.shift_enter_unavailable());
}

// -- detect_terminal_brand_from_env (pure) --------------------------------

#[test]
fn brand_ghostty_from_term_program() {
    let env = env_from(&[("TERM_PROGRAM", "Ghostty")]);
    assert_eq!(detect_terminal_brand_from_env(&env), TerminalName::Ghostty);
}

#[test]
fn brand_wezterm_from_version_var() {
    let env = env_from(&[("WEZTERM_VERSION", "20240203")]);
    assert_eq!(detect_terminal_brand_from_env(&env), TerminalName::WezTerm);
}

#[test]
fn brand_iterm2_from_session_id() {
    let env = env_from(&[("ITERM_SESSION_ID", "w0t0p0")]);
    assert_eq!(detect_terminal_brand_from_env(&env), TerminalName::Iterm2);
}

#[test]
fn brand_iterm2_from_lc_terminal_over_ssh() {
    // Only LC_TERMINAL survives the SSH hop; ITERM_SESSION_ID / TERM_PROGRAM do not.
    let env = env_from(&[("LC_TERMINAL", "iTerm2")]);
    assert_eq!(detect_terminal_brand_from_env(&env), TerminalName::Iterm2);
}

#[test]
fn brand_not_iterm2_for_foreign_lc_terminal() {
    // Value-exact gate: a non-iTerm2 LC_TERMINAL value must NOT match
    // (matches on value, not bare presence).
    let env = env_from(&[("LC_TERMINAL", "not-iterm2")]);
    assert_eq!(detect_terminal_brand_from_env(&env), TerminalName::Unknown);
}

#[test]
fn brand_kitty_from_term() {
    let env = env_from(&[("TERM", "xterm-kitty")]);
    assert_eq!(detect_terminal_brand_from_env(&env), TerminalName::Kitty);
}

#[test]
fn brand_alacritty_from_term() {
    let env = env_from(&[("TERM", "alacritty")]);
    assert_eq!(
        detect_terminal_brand_from_env(&env),
        TerminalName::Alacritty
    );
}

#[test]
fn brand_rio_from_term() {
    let env = env_from(&[("TERM", "rio")]);
    assert_eq!(detect_terminal_brand_from_env(&env), TerminalName::Rio);
}

#[test]
fn brand_foot_from_term() {
    let env = env_from(&[("TERM", "foot")]);
    assert_eq!(detect_terminal_brand_from_env(&env), TerminalName::Foot);
}

#[test]
fn brand_foot_extra_from_term() {
    let env = env_from(&[("TERM", "foot-extra")]);
    assert_eq!(detect_terminal_brand_from_env(&env), TerminalName::Foot);
}

#[test]
fn brand_foot_direct_from_term() {
    let env = env_from(&[("TERM", "foot-direct")]);
    assert_eq!(detect_terminal_brand_from_env(&env), TerminalName::Foot);
}

#[test]
fn brand_not_foot_for_unrelated_term() {
    // Over-match guard: a TERM merely containing "foot" is not foot.
    let env = env_from(&[("TERM", "xterm-footbar")]);
    assert_eq!(detect_terminal_brand_from_env(&env), TerminalName::Unknown);
}

#[test]
fn brand_not_foot_for_foot_prefixed_out_of_set_term() {
    // Inverse boundary: a `foot`-prefixed value outside the exact set
    // stays Unknown (pins the exact-set decision over `starts_with`).
    let env = env_from(&[("TERM", "foot-extra-256color")]);
    assert_eq!(detect_terminal_brand_from_env(&env), TerminalName::Unknown);
}

#[test]
fn brand_vte_from_vte_version() {
    let env = env_from(&[("VTE_VERSION", "7402")]);
    assert_eq!(detect_terminal_brand_from_env(&env), TerminalName::Vte);
}

#[test]
fn brand_terminator_from_term_program() {
    let env = env_from(&[("TERM_PROGRAM", "terminator")]);
    assert_eq!(
        detect_terminal_brand_from_env(&env),
        TerminalName::Terminator
    ); // WHY: canonical per detect-terminal
}

#[test]
fn terminator_vte_version_interaction() {
    let env = env_from(&[("TERM_PROGRAM", "terminator"), ("VTE_VERSION", "8200")]);
    let ctx = build_terminal_context_from_env(&env);
    assert_eq!(ctx.brand, TerminalName::Terminator);
    assert!(ctx.is_vte_based()); // WHY: helper covers version + brand
}

#[test]
fn terminator_with_tmux() {
    let env = env_from(&[("TERM_PROGRAM", "terminator"), ("TMUX", "/tmp/tmux")]);
    let ctx = build_terminal_context_from_env(&env);
    assert_eq!(ctx.brand, TerminalName::Terminator);
    assert_eq!(ctx.multiplexer, MultiplexerKind::Tmux);
}

#[test]
fn terminator_over_ssh() {
    let env = env_from(&[("TERM_PROGRAM", "terminator"), ("SSH_TTY", "/dev/pts/0")]);
    let ctx = build_terminal_context_from_env(&env);
    assert_eq!(ctx.brand, TerminalName::Terminator);
    assert!(ctx.is_ssh);
}

#[test]
fn terminator_focus_tracking() {
    let env = env_from(&[("TERM_PROGRAM", "terminator")]);
    let ctx = build_terminal_context_from_env(&env);
    assert!(!matches!(
        ctx.brand,
        TerminalName::AppleTerminal | TerminalName::Unknown
    )); // supports focus like VTE
    assert!(ctx.is_vte_based());
}

#[test]

fn brand_windows_terminal_from_wt_session() {
    let env = env_from(&[("WT_SESSION", "a1b2c3d4-e5f6-7890-abcd-ef1234567890")]);
    assert_eq!(
        detect_terminal_brand_from_env(&env),
        TerminalName::WindowsTerminal
    );
}

#[test]
fn brand_unknown_empty_env() {
    let env = env_from(&[]);
    assert_eq!(detect_terminal_brand_from_env(&env), TerminalName::Unknown);
}

// -- refine_unknown_brand_for_host ---------------------------------------

#[test]
fn refine_unknown_brand_defaults_to_wt_only_on_windows() {
    use super::TerminalName::{Unknown, VsCode, WindowsTerminal};
    use crate::host::HostOs::{Linux, Windows};
    let cases = [
        (Unknown, Windows, WindowsTerminal), // DefTerm handoff: no WT_SESSION
        (Unknown, Linux, Unknown),
        (VsCode, Windows, VsCode), // never override a positively detected brand
    ];
    for (brand, host, expected) in cases {
        assert_eq!(refine_unknown_brand_for_host(brand, host), expected);
    }
}

#[test]
fn env_brand_holds_raw_detection_independent_of_refinement() {
    // `build_*` records the raw detection in both fields.
    let ctx = build_terminal_context_from_env(&env_from(&[]));
    assert_eq!(ctx.brand, TerminalName::Unknown);
    assert_eq!(ctx.env_brand, TerminalName::Unknown);

    // The fallback refines only `brand`; `env_brand` stays raw so the glyphs
    // legacy-console check and shift_enter_unavailable still treat a bare
    // ConHost conservatively.
    let effective = refine_unknown_brand_for_host(ctx.brand, crate::host::HostOs::Windows);
    assert_eq!(effective, TerminalName::WindowsTerminal);
    assert_eq!(ctx.env_brand, TerminalName::Unknown);
}

#[test]
fn mouse_reporting_leaks_only_for_jetbrains_on_windows() {
    use crate::host::HostOs;
    // The reported failure: JediTerm on Windows leaks raw X10 mouse bytes.
    assert!(mouse_reporting_leaks(
        TerminalName::JetBrains,
        HostOs::Windows
    ));
    // JetBrains on macOS/Linux is fine — crossterm's unix parser decodes the
    // reports, so we must NOT downgrade those to minimal.
    assert!(!mouse_reporting_leaks(
        TerminalName::JetBrains,
        HostOs::Macos
    ));
    assert!(!mouse_reporting_leaks(
        TerminalName::JetBrains,
        HostOs::Linux
    ));
    // Other Windows terminals deliver native console mouse records — no leak.
    assert!(!mouse_reporting_leaks(
        TerminalName::WindowsTerminal,
        HostOs::Windows
    ));
    assert!(!mouse_reporting_leaks(TerminalName::Kitty, HostOs::Windows));
}

// -- detect_byobu_from_env ------------------------------------------------

#[test]
fn byobu_tmux_explicit_backend() {
    let env = env_from(&[("BYOBU_BACKEND", "tmux"), ("TMUX", "/tmp/tmux")]);
    assert_eq!(detect_byobu_from_env(&env), Some(ByobuBackend::Tmux));
}

#[test]
fn byobu_screen_explicit_backend() {
    let env = env_from(&[("BYOBU_BACKEND", "screen"), ("STY", "12345.pts-0")]);
    assert_eq!(detect_byobu_from_env(&env), Some(ByobuBackend::Screen));
}

#[test]
fn byobu_inferred_tmux_from_config_dir() {
    let env = env_from(&[
        ("BYOBU_CONFIG_DIR", "/home/user/.byobu"),
        ("TMUX", "/tmp/tmux"),
    ]);
    assert_eq!(detect_byobu_from_env(&env), Some(ByobuBackend::Tmux));
}

#[test]
fn byobu_inferred_screen_from_distro() {
    let env = env_from(&[("BYOBU_DISTRO", "Ubuntu"), ("STY", "12345.pts-0")]);
    assert_eq!(detect_byobu_from_env(&env), Some(ByobuBackend::Screen));
}

#[test]
fn byobu_markers_but_no_mux_markers_returns_none() {
    let env = env_from(&[("BYOBU_CONFIG_DIR", "/home/user/.byobu")]);
    assert_eq!(detect_byobu_from_env(&env), None);
}

#[test]
fn no_byobu_markers_returns_none() {
    let env = env_from(&[("TMUX", "/tmp/tmux")]);
    assert_eq!(detect_byobu_from_env(&env), None);
}

// -- detect_multiplexer_from_env ------------------------------------------

#[test]
fn mux_plain_tmux() {
    let env = env_from(&[("TMUX", "/tmp/tmux-501/default,12345,0")]);
    assert_eq!(detect_multiplexer_from_env(&env), MultiplexerKind::Tmux);
}

#[test]
fn mux_plain_screen() {
    let env = env_from(&[("STY", "12345.pts-0.host")]);
    assert_eq!(detect_multiplexer_from_env(&env), MultiplexerKind::Screen);
}

#[test]
fn mux_zellij() {
    let env = env_from(&[("ZELLIJ", "0")]);
    assert_eq!(detect_multiplexer_from_env(&env), MultiplexerKind::Zellij);
}

#[test]
fn mux_zellij_from_session_name() {
    let env = env_from(&[("ZELLIJ_SESSION_NAME", "my-session")]);
    assert_eq!(detect_multiplexer_from_env(&env), MultiplexerKind::Zellij);
}

#[test]
fn mux_none_empty_env() {
    let env = env_from(&[]);
    assert_eq!(
        detect_multiplexer_from_env(&env),
        MultiplexerKind::Undetected
    );
}

#[test]
fn mux_cmux_from_socket_path() {
    let env = env_from(&[("CMUX_SOCKET_PATH", "/tmp/cmux.sock")]);
    assert_eq!(detect_multiplexer_from_env(&env), MultiplexerKind::Cmux);
}

#[test]
fn mux_cmux_from_panel_id() {
    let env = env_from(&[("CMUX_PANEL_ID", "1")]);
    assert_eq!(detect_multiplexer_from_env(&env), MultiplexerKind::Cmux);
}

#[test]
fn mux_cmux_from_bundle_id() {
    let env = env_from(&[("CMUX_BUNDLE_ID", "com.cmuxterm.app")]);
    assert_eq!(detect_multiplexer_from_env(&env), MultiplexerKind::Cmux);
}

#[test]
fn mux_empty_cmux_socket_alone_is_undetected() {
    // CMUX_SOCKET may be present but empty under cmux; env_get filters empties.
    // Without a non-empty CMUX_* marker, do not classify as Cmux.
    let env = env_from(&[("CMUX_SOCKET", "")]);
    assert_eq!(
        detect_multiplexer_from_env(&env),
        MultiplexerKind::Undetected
    );
}

#[test]
fn mux_tmux_nested_inside_cmux_wins() {
    let env = env_from(&[
        ("TMUX", "/tmp/tmux-501/default,12345,0"),
        ("CMUX_SOCKET_PATH", "/tmp/cmux.sock"),
        ("CMUX_PANEL_ID", "1"),
    ]);
    assert_eq!(detect_multiplexer_from_env(&env), MultiplexerKind::Tmux);
}

#[test]
fn mux_herdr_from_herdr_env() {
    let env = env_from(&[("HERDR_ENV", "1")]);
    assert_eq!(detect_multiplexer_from_env(&env), MultiplexerKind::Herdr);
}

#[test]
fn mux_tmux_nested_inside_herdr_wins() {
    // tmux started inside a herdr pane, or a stale TMUX frozen into the pane by
    // herdr's daemon — indistinguishable from the env, and tmux wins either way.
    let env = env_from(&[
        ("TMUX", "/tmp/tmux-501/default,12345,0"),
        ("HERDR_ENV", "1"),
    ]);
    assert_eq!(detect_multiplexer_from_env(&env), MultiplexerKind::Tmux);
}

#[test]
fn mux_herdr_nested_inside_cmux_wins() {
    // herdr in a cmux panel inherits the CMUX_* markers; the inner layer wins.
    let env = env_from(&[
        ("CMUX_SOCKET_PATH", "/tmp/cmux.sock"),
        ("CMUX_PANEL_ID", "1"),
        ("HERDR_ENV", "1"),
    ]);
    assert_eq!(detect_multiplexer_from_env(&env), MultiplexerKind::Herdr);
}

// -- ambiguous marker precedence ------------------------------------------

#[test]
fn tmux_beats_zellij_when_both_set() {
    // tmux pane launched from inside Zellij leaves inherited ZELLIJ var.
    let env = env_from(&[("TMUX", "/tmp/tmux-501/default,12345,0"), ("ZELLIJ", "0")]);
    assert_eq!(detect_multiplexer_from_env(&env), MultiplexerKind::Tmux);
}

#[test]
fn byobu_screen_beats_tmux_marker() {
    // BYOBU_BACKEND=screen with a stale TMUX var — Byobu backend wins.
    let env = env_from(&[
        ("BYOBU_BACKEND", "screen"),
        ("TMUX", "/tmp/tmux-old"),
        ("STY", "12345.pts-0"),
    ]);
    assert_eq!(detect_multiplexer_from_env(&env), MultiplexerKind::Screen);
}

#[test]
fn byobu_tmux_explicit_with_sty_stays_tmux() {
    // BYOBU_BACKEND=tmux + stale STY — Byobu backend wins.
    let env = env_from(&[
        ("BYOBU_BACKEND", "tmux"),
        ("STY", "old-screen"),
        ("TMUX", "/tmp/tmux"),
    ]);
    assert_eq!(detect_multiplexer_from_env(&env), MultiplexerKind::Tmux);
}

// -- detect_tmux_meta_from_env --------------------------------------------

#[test]
fn tmux_meta_populated() {
    let env = env_from(&[
        ("TMUX", "/tmp/tmux-501/default,12345,0"),
        ("TMUX_PANE", "%3"),
    ]);
    let meta = detect_tmux_meta_from_env(&env);
    assert_eq!(
        meta.tmux_env.as_deref(),
        Some("/tmp/tmux-501/default,12345,0")
    );
    assert_eq!(meta.tmux_pane.as_deref(), Some("%3"));
}

#[test]
fn tmux_meta_empty_outside_tmux() {
    let env = env_from(&[]);
    let meta = detect_tmux_meta_from_env(&env);
    assert_eq!(meta, TmuxClientMeta::default());
}

// -- build_terminal_context_from_env (integration) ------------------------

#[test]
fn standalone_context_does_not_run_live_tmux_queries() {
    let env = env_from(&[("TMUX", "/tmp/definitely-missing-tmux-server")]);
    let ctx = standalone_terminal_context_from_env(&env, crate::host::HostOs::Linux);
    assert_eq!(ctx.multiplexer, MultiplexerKind::Tmux);
    assert_eq!(ctx.tmux_version, None);
    assert_eq!(ctx.tmux_extended_keys, None);
}

#[test]
fn context_plain_terminal() {
    let env = env_from(&[("TERM_PROGRAM", "Ghostty")]);
    let ctx = build_terminal_context_from_env(&env);
    assert_eq!(ctx.brand, TerminalName::Ghostty);
    assert_eq!(ctx.multiplexer, MultiplexerKind::Undetected);
    assert_eq!(ctx.byobu, None);
    assert!(!ctx.is_tmux_backed());
    assert!(!ctx.is_byobu());
}

#[test]
fn context_iterm2_lc_terminal_version_fallback_over_ssh() {
    // Over SSH, TERM_PROGRAM_VERSION is stripped; version gating falls back to
    // iTerm2's SSH-surviving LC_TERMINAL_VERSION.
    let env = env_from(&[("LC_TERMINAL", "iTerm2"), ("LC_TERMINAL_VERSION", "3.5.6")]);
    let ctx = build_terminal_context_from_env(&env);
    assert_eq!(ctx.brand, TerminalName::Iterm2);
    assert_eq!(ctx.term_program_version.as_deref(), Some("3.5.6"));
}

#[test]
fn context_plain_tmux() {
    let env = env_from(&[
        ("TERM_PROGRAM", "iTerm2"),
        ("TMUX", "/tmp/tmux-501/default,12345,0"),
        ("TMUX_PANE", "%0"),
    ]);
    let ctx = build_terminal_context_from_env(&env);
    assert_eq!(ctx.brand, TerminalName::Iterm2);
    assert_eq!(ctx.multiplexer, MultiplexerKind::Tmux);
    assert_eq!(ctx.byobu, None);
    assert!(ctx.is_tmux_backed());
    assert!(!ctx.is_byobu());
    assert_eq!(ctx.tmux_config_path(), "~/.tmux.conf");
    assert_eq!(
        ctx.tmux_meta.tmux_env.as_deref(),
        Some("/tmp/tmux-501/default,12345,0")
    );
}

#[test]
fn context_byobu_tmux() {
    let env = env_from(&[
        ("BYOBU_BACKEND", "tmux"),
        ("TMUX", "/tmp/tmux-501/default,12345,0"),
        ("TMUX_PANE", "%1"),
    ]);
    let ctx = build_terminal_context_from_env(&env);
    assert_eq!(ctx.multiplexer, MultiplexerKind::Tmux);
    assert_eq!(ctx.byobu, Some(ByobuBackend::Tmux));
    assert!(ctx.is_tmux_backed());
    assert!(ctx.is_byobu());
    assert_eq!(ctx.tmux_config_path(), "~/.byobu/.tmux.conf");
}

#[test]
fn context_byobu_screen() {
    let env = env_from(&[("BYOBU_BACKEND", "screen"), ("STY", "12345.pts-0.host")]);
    let ctx = build_terminal_context_from_env(&env);
    assert_eq!(ctx.multiplexer, MultiplexerKind::Screen);
    assert_eq!(ctx.byobu, Some(ByobuBackend::Screen));
    assert!(!ctx.is_tmux_backed());
    assert!(ctx.is_byobu());
}

#[test]
fn context_plain_screen_no_byobu() {
    let env = env_from(&[("STY", "12345.pts-0.host")]);
    let ctx = build_terminal_context_from_env(&env);
    assert_eq!(ctx.multiplexer, MultiplexerKind::Screen);
    assert_eq!(ctx.byobu, None);
    assert!(!ctx.is_byobu());
}

#[test]
fn context_zellij() {
    let env = env_from(&[
        ("ZELLIJ", "0"),
        ("ZELLIJ_SESSION_NAME", "my-session"),
        ("TERM_PROGRAM", "Ghostty"),
    ]);
    let ctx = build_terminal_context_from_env(&env);
    assert_eq!(ctx.brand, TerminalName::Ghostty);
    assert_eq!(ctx.multiplexer, MultiplexerKind::Zellij);
    assert_eq!(ctx.byobu, None);
    assert!(!ctx.is_tmux_backed());
}

#[test]
fn context_tmux_inside_zellij() {
    // tmux launched from Zellij: TMUX is set, inherited ZELLIJ var remains.
    let env = env_from(&[("TMUX", "/tmp/tmux-501/default,12345,0"), ("ZELLIJ", "0")]);
    let ctx = build_terminal_context_from_env(&env);
    assert_eq!(ctx.multiplexer, MultiplexerKind::Tmux);
    assert!(ctx.is_tmux_backed());
}

#[test]
fn context_tmux_meta_not_populated_for_non_tmux() {
    let env = env_from(&[("ZELLIJ", "0")]);
    let ctx = build_terminal_context_from_env(&env);
    assert_eq!(ctx.tmux_meta, TmuxClientMeta::default());
}

#[test]
fn context_ambiguous_byobu_screen_with_tmux() {
    // BYOBU_BACKEND=screen + stale TMUX → screen wins via Byobu backend.
    let env = env_from(&[
        ("BYOBU_BACKEND", "screen"),
        ("TMUX", "/tmp/tmux-old"),
        ("STY", "12345.pts-0"),
    ]);
    let ctx = build_terminal_context_from_env(&env);
    assert_eq!(ctx.multiplexer, MultiplexerKind::Screen);
    assert_eq!(ctx.byobu, Some(ByobuBackend::Screen));
}

#[test]
fn context_empty_env_values_ignored() {
    // Empty string values should not trigger detection.
    let env = env_from(&[("TMUX", ""), ("ZELLIJ", ""), ("STY", "")]);
    let ctx = build_terminal_context_from_env(&env);
    assert_eq!(ctx.multiplexer, MultiplexerKind::Undetected);
}

// =====================================================================
// determine_alt_screen_policy: fullscreen policy matrix
// =====================================================================

fn plain_ctx() -> TerminalContext {
    TerminalContext {
        brand: TerminalName::Ghostty,
        multiplexer: MultiplexerKind::Undetected,
        ..Default::default()
    }
}

fn tmux_ctx() -> TerminalContext {
    TerminalContext {
        brand: TerminalName::Iterm2,
        multiplexer: MultiplexerKind::Tmux,
        tmux_meta: TmuxClientMeta {
            tmux_env: Some("/tmp/tmux-501/default,12345,0".to_owned()),
            tmux_pane: Some("%0".to_owned()),
        },
        ..Default::default()
    }
}

fn zellij_ctx_test() -> TerminalContext {
    TerminalContext {
        brand: TerminalName::Ghostty,
        multiplexer: MultiplexerKind::Zellij,
        ..Default::default()
    }
}

fn screen_ctx() -> TerminalContext {
    TerminalContext {
        brand: TerminalName::Unknown,
        multiplexer: MultiplexerKind::Screen,
        ..Default::default()
    }
}

fn byobu_tmux_ctx() -> TerminalContext {
    TerminalContext {
        brand: TerminalName::Unknown,
        multiplexer: MultiplexerKind::Tmux,
        byobu: Some(ByobuBackend::Tmux),
        tmux_meta: TmuxClientMeta {
            tmux_env: Some("/tmp/tmux".to_owned()),
            tmux_pane: Some("%1".to_owned()),
        },
        ..Default::default()
    }
}

fn byobu_screen_ctx() -> TerminalContext {
    TerminalContext {
        brand: TerminalName::Unknown,
        multiplexer: MultiplexerKind::Screen,
        byobu: Some(ByobuBackend::Screen),
        ..Default::default()
    }
}

// -- Alt-screen policy matrix (all modes × contexts × CLI override) -------

#[derive(Debug)]
struct AltScreenCase {
    name: &'static str,
    cli_no_alt: bool,
    mode: AltScreenMode,
    ctx: TerminalContext,
    control: bool,
    expected: bool,
}

#[test]
fn alt_screen_policy_matrix() {
    let cases = [
        // Auto mode
        AltScreenCase {
            name: "auto_plain_fullscreen",
            cli_no_alt: false,
            mode: AltScreenMode::Auto,
            ctx: plain_ctx(),
            control: false,
            expected: true,
        },
        AltScreenCase {
            name: "auto_tmux_fullscreen",
            cli_no_alt: false,
            mode: AltScreenMode::Auto,
            ctx: tmux_ctx(),
            control: false,
            expected: true,
        },
        AltScreenCase {
            name: "auto_tmux_control_inline",
            cli_no_alt: false,
            mode: AltScreenMode::Auto,
            ctx: tmux_ctx(),
            control: true,
            expected: false,
        },
        AltScreenCase {
            name: "auto_zellij_inline",
            cli_no_alt: false,
            mode: AltScreenMode::Auto,
            ctx: zellij_ctx_test(),
            control: false,
            expected: false,
        },
        AltScreenCase {
            name: "auto_screen_fullscreen",
            cli_no_alt: false,
            mode: AltScreenMode::Auto,
            ctx: screen_ctx(),
            control: false,
            expected: true,
        },
        AltScreenCase {
            name: "auto_byobu_tmux_fullscreen",
            cli_no_alt: false,
            mode: AltScreenMode::Auto,
            ctx: byobu_tmux_ctx(),
            control: false,
            expected: true,
        },
        AltScreenCase {
            name: "auto_byobu_screen_fullscreen",
            cli_no_alt: false,
            mode: AltScreenMode::Auto,
            ctx: byobu_screen_ctx(),
            control: false,
            expected: true,
        },
        AltScreenCase {
            name: "auto_byobu_tmux_control_inline",
            cli_no_alt: false,
            mode: AltScreenMode::Auto,
            ctx: byobu_tmux_ctx(),
            control: true,
            expected: false,
        },
        // Never mode: always inline
        AltScreenCase {
            name: "never_plain_inline",
            cli_no_alt: false,
            mode: AltScreenMode::Never,
            ctx: plain_ctx(),
            control: false,
            expected: false,
        },
        AltScreenCase {
            name: "never_tmux_inline",
            cli_no_alt: false,
            mode: AltScreenMode::Never,
            ctx: tmux_ctx(),
            control: false,
            expected: false,
        },
        AltScreenCase {
            name: "never_screen_inline",
            cli_no_alt: false,
            mode: AltScreenMode::Never,
            ctx: screen_ctx(),
            control: false,
            expected: false,
        },
        AltScreenCase {
            name: "never_zellij_inline",
            cli_no_alt: false,
            mode: AltScreenMode::Never,
            ctx: zellij_ctx_test(),
            control: false,
            expected: false,
        },
        AltScreenCase {
            name: "never_byobu_tmux_inline",
            cli_no_alt: false,
            mode: AltScreenMode::Never,
            ctx: byobu_tmux_ctx(),
            control: false,
            expected: false,
        },
        AltScreenCase {
            name: "never_byobu_screen_inline",
            cli_no_alt: false,
            mode: AltScreenMode::Never,
            ctx: byobu_screen_ctx(),
            control: false,
            expected: false,
        },
        // Always mode: forces fullscreen
        AltScreenCase {
            name: "always_plain_fullscreen",
            cli_no_alt: false,
            mode: AltScreenMode::Always,
            ctx: plain_ctx(),
            control: false,
            expected: true,
        },
        AltScreenCase {
            name: "always_tmux_fullscreen",
            cli_no_alt: false,
            mode: AltScreenMode::Always,
            ctx: tmux_ctx(),
            control: false,
            expected: true,
        },
        AltScreenCase {
            name: "always_tmux_control_fullscreen",
            cli_no_alt: false,
            mode: AltScreenMode::Always,
            ctx: tmux_ctx(),
            control: true,
            expected: true,
        },
        AltScreenCase {
            name: "always_zellij_fullscreen",
            cli_no_alt: false,
            mode: AltScreenMode::Always,
            ctx: zellij_ctx_test(),
            control: false,
            expected: true,
        },
        AltScreenCase {
            name: "always_screen_fullscreen",
            cli_no_alt: false,
            mode: AltScreenMode::Always,
            ctx: screen_ctx(),
            control: false,
            expected: true,
        },
        AltScreenCase {
            name: "always_byobu_screen_fullscreen",
            cli_no_alt: false,
            mode: AltScreenMode::Always,
            ctx: byobu_screen_ctx(),
            control: false,
            expected: true,
        },
        // CLI --no-alt-screen overrides everything
        AltScreenCase {
            name: "cli_no_alt_overrides_auto",
            cli_no_alt: true,
            mode: AltScreenMode::Auto,
            ctx: plain_ctx(),
            control: false,
            expected: false,
        },
        AltScreenCase {
            name: "cli_no_alt_overrides_always",
            cli_no_alt: true,
            mode: AltScreenMode::Always,
            ctx: plain_ctx(),
            control: false,
            expected: false,
        },
        AltScreenCase {
            name: "cli_no_alt_overrides_never",
            cli_no_alt: true,
            mode: AltScreenMode::Never,
            ctx: plain_ctx(),
            control: false,
            expected: false,
        },
        AltScreenCase {
            name: "cli_no_alt_overrides_auto_in_tmux",
            cli_no_alt: true,
            mode: AltScreenMode::Auto,
            ctx: tmux_ctx(),
            control: false,
            expected: false,
        },
        AltScreenCase {
            name: "cli_no_alt_overrides_always_in_zellij",
            cli_no_alt: true,
            mode: AltScreenMode::Always,
            ctx: zellij_ctx_test(),
            control: false,
            expected: false,
        },
        AltScreenCase {
            name: "cli_no_alt_overrides_always_in_tmux_control",
            cli_no_alt: true,
            mode: AltScreenMode::Always,
            ctx: tmux_ctx(),
            control: true,
            expected: false,
        },
    ];

    for case in &cases {
        let result =
            determine_alt_screen_policy(case.cli_no_alt, case.mode, &case.ctx, case.control);
        assert_eq!(
            result, case.expected,
            "alt_screen_policy failed on case '{}'",
            case.name
        );
    }
}

// -- Windows Terminal context integration ---------------------------------

#[test]
fn context_windows_terminal() {
    let env = env_from(&[("WT_SESSION", "a1b2c3d4-e5f6-7890-abcd-ef1234567890")]);
    let ctx = build_terminal_context_from_env(&env);
    assert_eq!(ctx.brand, TerminalName::WindowsTerminal);
    assert_eq!(ctx.multiplexer, MultiplexerKind::Undetected);
    assert!(!ctx.is_ssh);
}

// -- Terminal brand detection edge cases -----------------------------------

// =====================================================================
// Extended environment matrix (final hardening)
// =====================================================================

// -- Byobu-screen: auto keeps fullscreen (screen is not auto-disabled) ----

#[test]
fn auto_byobu_screen_is_fullscreen() {
    let ctx = TerminalContext {
        brand: TerminalName::Unknown,
        multiplexer: MultiplexerKind::Screen,
        byobu: Some(ByobuBackend::Screen),
        ..Default::default()
    };
    assert!(determine_alt_screen_policy(
        false,
        AltScreenMode::Auto,
        &ctx,
        false
    ));
}

// -- Terminal brand detection edge cases -----------------------------------

#[test]
fn brand_vscode_from_term_program() {
    let env = env_from(&[("TERM_PROGRAM", "vscode")]);
    assert_eq!(detect_terminal_brand_from_env(&env), TerminalName::VsCode);
}

#[test]
fn brand_kitty_from_window_id() {
    let env = env_from(&[("KITTY_WINDOW_ID", "1")]);
    assert_eq!(detect_terminal_brand_from_env(&env), TerminalName::Kitty);
}

#[test]
fn brand_alacritty_from_socket() {
    let env = env_from(&[("ALACRITTY_SOCKET", "/tmp/alacritty.sock")]);
    assert_eq!(
        detect_terminal_brand_from_env(&env),
        TerminalName::Alacritty
    );
}

#[test]
fn brand_apple_terminal_from_session_id() {
    let env = env_from(&[("TERM_SESSION_ID", "w0t0p0:ABC123")]);
    assert_eq!(
        detect_terminal_brand_from_env(&env),
        TerminalName::AppleTerminal
    );
}

#[test]
fn brand_term_program_takes_precedence_over_other_vars() {
    // TERM_PROGRAM should win even when other brand-specific vars are set.
    let env = env_from(&[
        ("TERM_PROGRAM", "Ghostty"),
        ("WEZTERM_VERSION", "20240203"),
        ("ITERM_SESSION_ID", "w0t0p0"),
    ]);
    assert_eq!(detect_terminal_brand_from_env(&env), TerminalName::Ghostty);
}

// -- IDE family detection (VS Code forks / xterm.js embeds) ---------------

#[test]
fn brand_cursor_from_cursor_trace_id() {
    // Cursor sets a unique CURSOR_TRACE_ID; it also sets TERM_PROGRAM=vscode
    // (since it's a VS Code fork), so we must detect it before the
    // TERM_PROGRAM lookup.
    let env = env_from(&[
        ("CURSOR_TRACE_ID", "abcdef0123456789"),
        ("TERM_PROGRAM", "vscode"),
    ]);
    assert_eq!(detect_terminal_brand_from_env(&env), TerminalName::Cursor);
}

#[test]
fn brand_cursor_from_askpass_main() {
    // Cursor exposes VSCODE_GIT_ASKPASS_MAIN pointing at its install path.
    let env = env_from(&[
        (
            "VSCODE_GIT_ASKPASS_MAIN",
            "/Applications/Cursor.app/Contents/Resources/app/extensions/git/dist/askpass-main.js",
        ),
        ("TERM_PROGRAM", "vscode"),
    ]);
    assert_eq!(detect_terminal_brand_from_env(&env), TerminalName::Cursor);
}

#[test]
fn brand_windsurf_from_askpass_main() {
    // Another VS Code fork; same askpass path substring pattern.
    let env = env_from(&[
        (
            "VSCODE_GIT_ASKPASS_MAIN",
            "/Applications/Windsurf.app/Contents/Resources/app/extensions/git/dist/askpass-main.js",
        ),
        ("TERM_PROGRAM", "vscode"),
    ]);
    assert_eq!(detect_terminal_brand_from_env(&env), TerminalName::Windsurf);
}

#[test]
fn brand_zed_from_term_program() {
    let env = env_from(&[("TERM_PROGRAM", "zed")]);
    assert_eq!(detect_terminal_brand_from_env(&env), TerminalName::Zed);
}

#[test]
fn brand_vscode_when_no_ide_markers() {
    // Pure VS Code: TERM_PROGRAM=vscode and no CURSOR_TRACE_ID / matching
    // askpass marker. Should remain VsCode, not collapse to anything else.
    let env = env_from(&[("TERM_PROGRAM", "vscode")]);
    assert_eq!(detect_terminal_brand_from_env(&env), TerminalName::VsCode);
}

#[test]
fn brand_vscode_when_askpass_does_not_match_ide() {
    // Pure VS Code askpass path (no "cursor"/"windsurf") — brand is VsCode
    // even when TERM_PROGRAM is also set.
    let env = env_from(&[
        ("VSCODE_GIT_ASKPASS_MAIN", "/usr/local/bin/askpass-main.js"),
        ("TERM_PROGRAM", "vscode"),
    ]);
    assert_eq!(detect_terminal_brand_from_env(&env), TerminalName::VsCode);
}

#[test]
fn brand_vscode_from_askpass_without_term_program() {
    // Remote SSH / tmux: TERM_PROGRAM missing or overwritten, but the VS Code
    // remote agent still injects VSCODE_GIT_ASKPASS_MAIN into the pane env.
    let env = env_from(&[(
        "VSCODE_GIT_ASKPASS_MAIN",
        "/home/user/.vscode-server/bin/abc/askpass",
    )]);
    assert_eq!(detect_terminal_brand_from_env(&env), TerminalName::VsCode);
}

#[test]
fn context_official_vscode_remote_from_askpass_and_ssh() {
    for server_dir in [".vscode-server", ".vscode-server-insiders"] {
        let askpass = format!("/home/user/{server_dir}/bin/abc/askpass");
        let env = env_from(&[
            ("VSCODE_GIT_ASKPASS_MAIN", &askpass),
            ("SSH_CONNECTION", "192.0.2.1 50000 192.0.2.2 22"),
        ]);
        let ctx = build_terminal_context_from_env(&env);
        assert_eq!(ctx.brand, TerminalName::VsCode);
        assert!(ctx.is_ssh);
        assert!(ctx.is_official_vscode_remote, "{server_dir}");
    }
}

#[test]
fn context_unofficial_vscode_remote_markers_are_not_official() {
    for askpass in [
        "/home/user/.vscode-server-oss/bin/abc/askpass",
        "/home/user/.vscodium-server/bin/abc/askpass",
        "/home/user/.code-oss-server/bin/abc/askpass",
        "/home/user/cache/.vscode-server-oss/.vscode-serverish/askpass",
        "/usr/local/bin/askpass-main.js",
    ] {
        let env = env_from(&[
            ("VSCODE_GIT_ASKPASS_MAIN", askpass),
            ("SSH_CONNECTION", "192.0.2.1 50000 192.0.2.2 22"),
        ]);
        let ctx = build_terminal_context_from_env(&env);
        assert_eq!(ctx.brand, TerminalName::VsCode);
        assert!(ctx.is_ssh);
        assert!(!ctx.is_official_vscode_remote, "{askpass}");
    }
}

#[test]
fn official_vscode_server_marker_without_ssh_is_not_remote() {
    let env = env_from(&[(
        "VSCODE_GIT_ASKPASS_MAIN",
        "/home/user/.vscode-server/bin/abc/askpass",
    )]);
    let ctx = build_terminal_context_from_env(&env);
    assert!(!ctx.is_ssh);
    assert!(!ctx.is_official_vscode_remote);
}

// -- Zellij detection from ZELLIJ_VERSION (no ZELLIJ or SESSION_NAME) -----

#[test]
fn mux_zellij_not_from_version_only() {
    // ZELLIJ_VERSION alone does not trigger Zellij detection — it must be
    // ZELLIJ or ZELLIJ_SESSION_NAME.
    let env = env_from(&[("ZELLIJ_VERSION", "0.43.1")]);
    assert_eq!(
        detect_multiplexer_from_env(&env),
        MultiplexerKind::Undetected
    );
}

#[test]
fn zellij_version_is_never_the_terminal_version() {
    // The multiplexer's version must never be attributed to the emulator.
    let env = env_from(&[
        ("TERM", "alacritty"),
        ("ZELLIJ", "0"),
        ("ZELLIJ_SESSION_NAME", "main"),
        ("ZELLIJ_VERSION", "0.43.1"),
    ]);
    let ctx = build_terminal_context_from_env(&env);
    assert_eq!(ctx.brand, TerminalName::Alacritty);
    assert_eq!(ctx.multiplexer, MultiplexerKind::Zellij);
    assert_eq!(ctx.term_version(), (String::new(), TermVersionSource::None));
}

// -- Byobu inference edge cases -------------------------------------------

#[test]
fn byobu_unknown_backend_string_with_tmux() {
    // Unknown BYOBU_BACKEND value falls through to mux-marker inference.
    let env = env_from(&[("BYOBU_BACKEND", "unknown"), ("TMUX", "/tmp/tmux")]);
    assert_eq!(detect_byobu_from_env(&env), Some(ByobuBackend::Tmux));
}

#[test]
fn byobu_unknown_backend_string_with_sty() {
    let env = env_from(&[("BYOBU_BACKEND", "fish"), ("STY", "12345.pts-0")]);
    assert_eq!(detect_byobu_from_env(&env), Some(ByobuBackend::Screen));
}

#[test]
fn byobu_unknown_backend_no_mux_returns_none() {
    let env = env_from(&[("BYOBU_BACKEND", "unknown")]);
    assert_eq!(detect_byobu_from_env(&env), None);
}

// -- Context-level edge cases ---------------------------------------------

#[test]
fn context_sty_takes_screen_when_no_tmux_or_zellij() {
    let env = env_from(&[("STY", "12345.pts-0.host")]);
    let ctx = build_terminal_context_from_env(&env);
    assert_eq!(ctx.multiplexer, MultiplexerKind::Screen);
    assert_eq!(ctx.byobu, None);
    assert!(!ctx.is_tmux_backed());
}

#[test]
fn context_screen_does_not_populate_tmux_meta() {
    let env = env_from(&[("STY", "12345.pts-0.host"), ("TMUX_PANE", "%stale")]);
    let ctx = build_terminal_context_from_env(&env);
    assert_eq!(ctx.multiplexer, MultiplexerKind::Screen);
    // tmux_meta should be default since multiplexer is not Tmux.
    assert_eq!(ctx.tmux_meta, TmuxClientMeta::default());
}

#[test]
fn context_tmux_config_path_is_standard_for_plain_tmux() {
    let env = env_from(&[("TMUX", "/tmp/tmux-501/default,1,0")]);
    let ctx = build_terminal_context_from_env(&env);
    assert_eq!(ctx.tmux_config_path(), "~/.tmux.conf");
}

#[test]
fn context_tmux_config_path_is_byobu_for_byobu_tmux() {
    let env = env_from(&[("BYOBU_BACKEND", "tmux"), ("TMUX", "/tmp/tmux")]);
    let ctx = build_terminal_context_from_env(&env);
    assert_eq!(ctx.tmux_config_path(), "~/.byobu/.tmux.conf");
}

#[test]
fn context_is_byobu_returns_true_for_both_backends() {
    let tmux_env = env_from(&[("BYOBU_BACKEND", "tmux"), ("TMUX", "/tmp/tmux")]);
    let screen_env = env_from(&[("BYOBU_BACKEND", "screen"), ("STY", "12345.pts-0")]);

    assert!(build_terminal_context_from_env(&tmux_env).is_byobu());
    assert!(build_terminal_context_from_env(&screen_env).is_byobu());
}

#[test]
fn context_is_byobu_returns_false_without_byobu_markers() {
    let env = env_from(&[("TMUX", "/tmp/tmux")]);
    assert!(!build_terminal_context_from_env(&env).is_byobu());
}

// =====================================================================
// parse_tmux_major_minor: version string parsing
// =====================================================================

#[test]
fn parse_tmux_version_standard() {
    assert_eq!(parse_tmux_major_minor("tmux 3.4"), Some((3, 4)));
}

#[test]
fn parse_tmux_version_with_letter_suffix() {
    assert_eq!(parse_tmux_major_minor("tmux 3.3a"), Some((3, 3)));
}

#[test]
fn parse_tmux_version_old() {
    assert_eq!(parse_tmux_major_minor("tmux 2.9"), Some((2, 9)));
}

#[test]
fn parse_tmux_version_next_gen() {
    assert_eq!(parse_tmux_major_minor("tmux 4.0"), Some((4, 0)));
}

#[test]
fn parse_tmux_version_missing_prefix() {
    assert_eq!(parse_tmux_major_minor("3.4"), None);
}

#[test]
fn parse_tmux_version_empty() {
    assert_eq!(parse_tmux_major_minor(""), None);
}

#[test]
fn parse_tmux_version_garbage() {
    assert_eq!(parse_tmux_major_minor("not a version"), None);
}

#[test]
fn parse_tmux_version_trailing_dot() {
    assert_eq!(parse_tmux_major_minor("tmux 3."), None);
}

#[test]
fn parse_tmux_version_no_minor() {
    assert_eq!(parse_tmux_major_minor("tmux 3"), None);
}

// =====================================================================
// parse_semver_major_minor: TERM_PROGRAM_VERSION parsing
// =====================================================================

#[test]
fn parse_semver_standard() {
    assert_eq!(parse_semver_major_minor("3.6.0"), Some((3, 6)));
}

#[test]
fn parse_semver_two_part() {
    assert_eq!(parse_semver_major_minor("1.2"), Some((1, 2)));
}

#[test]
fn parse_semver_high_version() {
    assert_eq!(parse_semver_major_minor("4.0.0"), Some((4, 0)));
}

#[test]
fn parse_semver_empty() {
    assert_eq!(parse_semver_major_minor(""), None);
}

#[test]
fn parse_semver_garbage() {
    assert_eq!(parse_semver_major_minor("garbage"), None);
}

#[test]
fn parse_semver_major_only() {
    assert_eq!(parse_semver_major_minor("3"), None);
}

// =====================================================================
// graphics_protocol_skip_reason

#[test]
fn graphics_protocol_skip_reason_tmux() {
    let ctx = TerminalContext {
        brand: TerminalName::Kitty,
        multiplexer: MultiplexerKind::Tmux,
        ..Default::default()
    };
    assert_eq!(ctx.graphics_protocol_skip_reason(), Some("tmux"));
}

#[test]
fn graphics_protocol_skip_reason_plain_kitty() {
    let ctx = TerminalContext {
        brand: TerminalName::Kitty,
        ..Default::default()
    };
    assert_eq!(ctx.graphics_protocol_skip_reason(), None);
}

// kitty_skip_reason: Kitty keyboard protocol skip-reason matrix
// =====================================================================

#[test]
fn kitty_skip_vscode() {
    let ctx = TerminalContext {
        brand: TerminalName::VsCode,
        ..Default::default()
    };
    assert_eq!(ctx.kitty_skip_reason(), Some("vscode"));
}

#[test]
fn kitty_skip_cursor() {
    let ctx = TerminalContext {
        brand: TerminalName::Cursor,
        ..Default::default()
    };
    assert_eq!(ctx.kitty_skip_reason(), Some("vscode"));
}

#[test]
fn kitty_skip_windsurf() {
    let ctx = TerminalContext {
        brand: TerminalName::Windsurf,
        ..Default::default()
    };
    assert_eq!(ctx.kitty_skip_reason(), Some("vscode"));
}

#[test]
fn kitty_skip_zed() {
    let ctx = TerminalContext {
        brand: TerminalName::Zed,
        ..Default::default()
    };
    assert_eq!(ctx.kitty_skip_reason(), Some("vscode"));
}

#[test]
fn kitty_skip_apple_terminal() {
    let ctx = TerminalContext {
        brand: TerminalName::AppleTerminal,
        ..Default::default()
    };
    assert_eq!(ctx.kitty_skip_reason(), Some("apple_terminal"));
}

#[test]
fn kitty_skip_screen() {
    let ctx = TerminalContext {
        multiplexer: MultiplexerKind::Screen,
        ..Default::default()
    };
    assert_eq!(ctx.kitty_skip_reason(), Some("screen"));
}

#[test]
fn kitty_skip_old_tmux() {
    let ctx = TerminalContext {
        multiplexer: MultiplexerKind::Tmux,
        tmux_version: Some("tmux 3.2".to_owned()),
        ..Default::default()
    };
    assert_eq!(ctx.kitty_skip_reason(), Some("tmux_old"));
}

#[test]
fn kitty_skip_tmux_no_version() {
    let ctx = TerminalContext {
        multiplexer: MultiplexerKind::Tmux,
        tmux_version: None,
        ..Default::default()
    };
    assert_eq!(ctx.kitty_skip_reason(), Some("tmux_old"));
}

#[test]
fn kitty_allowed_modern_tmux() {
    let ctx = TerminalContext {
        multiplexer: MultiplexerKind::Tmux,
        tmux_version: Some("tmux 3.3".to_owned()),
        ..Default::default()
    };
    assert_eq!(ctx.kitty_skip_reason(), None);
}

#[test]
fn kitty_allowed_modern_tmux_letter() {
    let ctx = TerminalContext {
        multiplexer: MultiplexerKind::Tmux,
        tmux_version: Some("tmux 3.3a".to_owned()),
        ..Default::default()
    };
    assert_eq!(ctx.kitty_skip_reason(), None);
}

#[test]
fn kitty_allowed_tmux_4() {
    let ctx = TerminalContext {
        multiplexer: MultiplexerKind::Tmux,
        tmux_version: Some("tmux 4.0".to_owned()),
        ..Default::default()
    };
    assert_eq!(ctx.kitty_skip_reason(), None);
}

#[test]
fn kitty_skip_unknown_terminal_no_multiplexer() {
    // Unknown brand with no multiplexer = no positive evidence of KKP
    // support. Catches VSCode-over-SSH, bare Docker containers, etc.
    let ctx = TerminalContext::default();
    assert_eq!(ctx.kitty_skip_reason(), Some("unknown_no_multiplexer"));
}

#[test]
fn kitty_allowed_known_good_terminal() {
    let ctx = TerminalContext {
        brand: TerminalName::Ghostty,
        ..Default::default()
    };
    assert_eq!(ctx.kitty_skip_reason(), None);
}

#[test]
fn kitty_allowed_foot_no_multiplexer() {
    // Core fix: a bare foot window negotiates KKP (no skip reason).
    let ctx = TerminalContext {
        brand: TerminalName::Foot,
        ..Default::default()
    };
    assert_eq!(ctx.kitty_skip_reason(), None);
}

#[test]
fn shift_enter_available_on_foot() {
    let ctx = TerminalContext {
        brand: TerminalName::Foot,
        env_brand: TerminalName::Foot,
        ..Default::default()
    };
    assert!(!ctx.shift_enter_unavailable());
}

#[test]
fn foot_ctrl_dot_is_reliable() {
    let ctx = TerminalContext {
        brand: TerminalName::Foot,
        ..Default::default()
    };
    assert!(!ctx.ctrl_dot_unreliable());
}

#[test]
fn kitty_skip_foot_over_old_tmux_uses_mux_logic() {
    let ctx = TerminalContext {
        brand: TerminalName::Foot,
        multiplexer: MultiplexerKind::Tmux,
        tmux_version: Some("tmux 3.0".to_owned()),
        ..Default::default()
    };
    assert_eq!(ctx.kitty_skip_reason(), Some("tmux_old"));
}

#[test]
fn kitty_allowed_zellij() {
    let ctx = TerminalContext {
        multiplexer: MultiplexerKind::Zellij,
        ..Default::default()
    };
    assert_eq!(ctx.kitty_skip_reason(), None);
}

#[test]
fn kitty_skip_byobu_screen() {
    let ctx = TerminalContext {
        multiplexer: MultiplexerKind::Screen,
        byobu: Some(ByobuBackend::Screen),
        ..Default::default()
    };
    assert_eq!(ctx.kitty_skip_reason(), Some("screen"));
}

#[test]
fn kitty_skip_windows_terminal() {
    let ctx = TerminalContext {
        brand: TerminalName::WindowsTerminal,
        ..Default::default()
    };
    assert_eq!(ctx.kitty_skip_reason(), Some("windows_terminal"));
}

#[test]
fn kitty_skip_vscode_over_tmux() {
    // When both brand and multiplexer would skip, brand wins.
    let ctx = TerminalContext {
        brand: TerminalName::VsCode,
        multiplexer: MultiplexerKind::Tmux,
        tmux_version: Some("tmux 3.0".to_owned()),
        ..Default::default()
    };
    assert_eq!(ctx.kitty_skip_reason(), Some("vscode"));
}

#[test]
fn kitty_skip_vte_version() {
    // VTE does not support Kitty keyboard protocol and crossterm's probe
    // can false-positive on it. https://gitlab.gnome.org/GNOME/vte/-/issues/2601
    let ctx = TerminalContext {
        vte_version: Some("7402".to_owned()),
        ..Default::default()
    };
    assert_eq!(ctx.kitty_skip_reason(), Some("vte"));
}

#[test]
fn kitty_skip_vte_brand() {
    let ctx = TerminalContext {
        brand: TerminalName::Vte,
        ..Default::default()
    };
    assert_eq!(ctx.kitty_skip_reason(), Some("vte"));
}

// =====================================================================
// shift_enter_unavailable: VTE version gating for Shift+Enter
// =====================================================================
//
// VTE 0.82.0 (= VTE_VERSION 8200) is the first release containing the
// Kitty keyboard protocol; earlier versions cannot distinguish
// Shift+Enter from bare Enter, so the UI should advertise Alt+Enter
// for newline insertion instead.

#[test]
fn shift_enter_unavailable_legacy_vte_version() {
    // VTE 0.64.2 (real user report) — well below the KKP cutoff.
    let ctx = TerminalContext {
        vte_version: Some("6402".to_owned()),
        ..Default::default()
    };
    assert!(ctx.shift_enter_unavailable());
}

#[test]
fn shift_enter_unavailable_just_below_cutoff() {
    // VTE 0.81.99 — anything below 8200 must return true.
    let ctx = TerminalContext {
        vte_version: Some("8199".to_owned()),
        ..Default::default()
    };
    assert!(ctx.shift_enter_unavailable());
}

#[test]
fn shift_enter_available_modern_vte() {
    // VTE 0.82.0 — first release with KKP.
    let ctx = TerminalContext {
        vte_version: Some("8200".to_owned()),
        ..Default::default()
    };
    assert!(!ctx.shift_enter_unavailable());
}

#[test]
fn shift_enter_available_future_vte() {
    // VTE 0.84.1 — well above the cutoff.
    let ctx = TerminalContext {
        vte_version: Some("8401".to_owned()),
        ..Default::default()
    };
    assert!(!ctx.shift_enter_unavailable());
}

#[test]
fn shift_enter_unavailable_vte_brand_no_version() {
    // Brand detected as VTE but VTE_VERSION missing — conservative: old.
    let ctx = TerminalContext {
        brand: TerminalName::Vte,
        vte_version: None,
        ..Default::default()
    };
    assert!(ctx.shift_enter_unavailable());
}

#[test]
fn shift_enter_unavailable_unparseable_version() {
    // Garbage in VTE_VERSION — conservative: old.
    let ctx = TerminalContext {
        brand: TerminalName::Vte,
        vte_version: Some("not-a-number".to_owned()),
        ..Default::default()
    };
    assert!(ctx.shift_enter_unavailable());
}

#[test]
fn shift_enter_available_kkp_terminals() {
    // Terminals that negotiate the Kitty keyboard protocol (or, for Apple
    // Terminal, recover the modifier via CoreGraphics) deliver a usable
    // Shift+Enter and must never report it as unavailable.
    for brand in [
        TerminalName::Ghostty,
        TerminalName::Kitty,
        TerminalName::WezTerm,
        TerminalName::AppleTerminal,
    ] {
        let ctx = TerminalContext {
            brand,
            env_brand: brand, // lockstep with brand (no Windows refinement)
            ..Default::default()
        };
        assert!(
            !ctx.shift_enter_unavailable(),
            "{brand:?} must not report shift_enter_unavailable"
        );
    }
}

#[test]
fn shift_enter_unavailable_vscode_family() {
    // VS Code's xterm.js (and VS Code-family / xterm.js IDE forks) never
    // negotiate KKP, so Shift+Enter arrives as a bare CR identical to
    // Enter. The UI must advertise Alt+Enter instead.
    for brand in [
        TerminalName::VsCode,
        TerminalName::Cursor,
        TerminalName::Windsurf,
        TerminalName::Zed,
    ] {
        let ctx = TerminalContext {
            brand,
            ..Default::default()
        };
        assert!(
            ctx.shift_enter_unavailable(),
            "{brand:?} (xterm.js) must report shift_enter_unavailable"
        );
    }
}

#[test]
fn shift_enter_unavailable_unknown_no_multiplexer() {
    // The common VS Code-over-SSH shape: TERM_PROGRAM isn't forwarded so
    // the brand falls back to Unknown, and the pager skips KKP. Shift+Enter
    // is indistinguishable from Enter — advertise Alt+Enter.
    let ctx = TerminalContext::default();
    assert_eq!(ctx.brand, TerminalName::Unknown);
    assert_eq!(ctx.env_brand, TerminalName::Unknown);
    assert_eq!(ctx.multiplexer, MultiplexerKind::Undetected);
    assert!(ctx.shift_enter_unavailable());
}

#[test]
fn shift_enter_unavailable_windows_refined_brand_stays_env_unknown() {
    // Native Windows DefTerm / bare ConHost: `brand` is refined to WT for
    // capabilities/label, but `env_brand` stays Unknown. KKP is skipped
    // (WT is in the skip list; ConHost has no KKP either), so Shift+Enter
    // is unreliable — advertise Alt+Enter, same as an unrefined Unknown.
    let ctx = TerminalContext {
        brand: TerminalName::WindowsTerminal,
        env_brand: TerminalName::Unknown,
        multiplexer: MultiplexerKind::Undetected,
        ..Default::default()
    };
    assert!(ctx.shift_enter_unavailable());
}

#[test]
fn shift_enter_available_positively_detected_windows_terminal() {
    // WT detected via WT_SESSION/TERM_PROGRAM: both brand fields are WT.
    // The Unknown gate must not fire; Shift+Enter follows the WT path
    // (KKP is skipped for WT, but that is a separate, pre-existing choice).
    let ctx = TerminalContext {
        brand: TerminalName::WindowsTerminal,
        env_brand: TerminalName::WindowsTerminal,
        multiplexer: MultiplexerKind::Undetected,
        ..Default::default()
    };
    assert!(!ctx.shift_enter_unavailable());
}

#[test]
fn shift_enter_available_unknown_with_multiplexer() {
    // Unknown brand but inside modern tmux: KKP is negotiated through the
    // multiplexer, so Shift+Enter works and must not be flagged.
    let ctx = TerminalContext {
        brand: TerminalName::Unknown,
        env_brand: TerminalName::Unknown,
        multiplexer: MultiplexerKind::Tmux,
        tmux_version: Some("tmux 3.4".to_owned()),
        ..Default::default()
    };
    assert!(!ctx.shift_enter_unavailable());
}

#[test]
fn herdr_over_ssh_pane_does_not_skip_kitty_keyboard() {
    // A real herdr pane reached over SSH: no TERM_PROGRAM, so the brand stays
    // Unknown. HERDR_ENV is what keeps this out of the unknown-no-multiplexer
    // skip, which would otherwise drop Shift+Enter (herdr speaks KKP).
    let env = env_from(&[
        ("HERDR_ENV", "1"),
        ("HERDR_PANE_ID", "3"),
        ("TERM", "xterm-256color"),
        ("COLORTERM", "truecolor"),
        ("SSH_CONNECTION", "10.0.0.1 52000 10.0.0.2 22"),
    ]);
    let ctx = build_terminal_context_from_env(&env);
    assert!(ctx.is_ssh);
    assert_eq!(ctx.brand, TerminalName::Unknown);
    // shift_enter_unavailable() reads env_brand, not brand.
    assert_eq!(ctx.env_brand, TerminalName::Unknown);
    assert_eq!(ctx.multiplexer, MultiplexerKind::Herdr);
    assert_eq!(ctx.kitty_skip_reason(), None);
    assert!(!ctx.shift_enter_unavailable());
}

#[test]
fn ctrl_dot_unreliable_on_vte() {
    let ctx = TerminalContext {
        brand: TerminalName::Vte,
        ..Default::default()
    };
    assert!(ctx.ctrl_dot_unreliable());
}

#[test]
fn ctrl_dot_unreliable_when_vte_version_set() {
    let ctx = TerminalContext {
        vte_version: Some("7402".to_owned()),
        ..Default::default()
    };
    assert!(ctx.ctrl_dot_unreliable());
}

#[test]
fn ctrl_dot_reliable_on_kitty() {
    let ctx = TerminalContext {
        brand: TerminalName::Kitty,
        ..Default::default()
    };
    assert!(!ctx.ctrl_dot_unreliable());
}

#[test]
fn ctrl_dot_unreliable_on_windows_terminal() {
    // WT propagates WT_SESSION into WSL via WSLENV, so this also covers
    // WSL-inside-WT — `Ctrl+.` can't survive WT's ConPTY pipeline either
    // way. (Pure WSL with no WT_SESSION is caught by `is_wsl()` in the
    // consumer, not here.)
    let ctx = TerminalContext {
        brand: TerminalName::WindowsTerminal,
        ..Default::default()
    };
    assert!(ctx.ctrl_dot_unreliable());
}

#[test]
fn ctrl_dot_unreliable_on_vscode_forks() {
    // VS Code's integrated terminal swallows `Ctrl+.` for its own command
    // palette. VS Code-family forks and other xterm.js IDE embeds inherit
    // the same behavior (forks share the keymap; others mirror it).
    // Preference is keyed off `kitty_skip_reason` ("vscode").
    for brand in [
        TerminalName::VsCode,
        TerminalName::Cursor,
        TerminalName::Windsurf,
        TerminalName::Zed,
    ] {
        let ctx = TerminalContext {
            brand,
            ..Default::default()
        };
        assert!(
            ctx.ctrl_dot_unreliable(),
            "expected ctrl_dot_unreliable on {brand:?}"
        );
    }
}

#[test]
fn ctrl_dot_unreliable_when_tmux_extended_keys_off() {
    // iTerm2+tmux still used to advertise Ctrl+. while KKP was
    // skipped for `extended-keys off`. Preference must follow the skip.
    let ctx = TerminalContext {
        brand: TerminalName::Iterm2,
        tmux_version: Some("tmux 3.6a".to_owned()),
        tmux_extended_keys: Some("off".to_owned()),
        ..tmux_ctx()
    };
    assert_eq!(ctx.kitty_skip_reason(), Some("tmux_extended_keys_off"));
    assert!(ctx.ctrl_dot_unreliable());
}

#[test]
fn ctrl_dot_reliable_on_iterm2_tmux_extended_keys_on() {
    let ctx = TerminalContext {
        brand: TerminalName::Iterm2,
        tmux_version: Some("tmux 3.6a".to_owned()),
        tmux_extended_keys: Some("on".to_owned()),
        ..tmux_ctx()
    };
    assert_eq!(ctx.kitty_skip_reason(), None);
    assert!(!ctx.ctrl_dot_unreliable());
}

#[test]
fn ctrl_dot_unreliable_on_unknown_no_multiplexer() {
    // Same KKP-skip path as VS Code SSH / unbranded hosts.
    let ctx = TerminalContext {
        brand: TerminalName::Unknown,
        multiplexer: MultiplexerKind::Undetected,
        ..Default::default()
    };
    assert_eq!(ctx.kitty_skip_reason(), Some("unknown_no_multiplexer"));
    assert!(ctx.ctrl_dot_unreliable());
}

// -- tmux extended-keys interaction with kitty_skip_reason ---------------

fn extended_keys_ctx(version: &str, extended_keys: Option<&str>) -> TerminalContext {
    TerminalContext {
        tmux_version: Some(version.to_owned()),
        tmux_extended_keys: extended_keys.map(str::to_owned),
        ..tmux_ctx()
    }
}

#[test]
fn kitty_skip_modern_tmux_extended_keys_off() {
    assert_eq!(
        extended_keys_ctx("tmux 3.4", Some("off")).kitty_skip_reason(),
        Some("tmux_extended_keys_off"),
    );
}

#[test]
fn kitty_allowed_modern_tmux_extended_keys_non_off_values() {
    // `on`/`always`/empty/uppercase/None must all NOT trigger the skip:
    // the comparison against `"off"` is exact and case-sensitive.
    for val in [Some("on"), Some("always"), Some(""), Some("OFF"), None] {
        assert_eq!(
            extended_keys_ctx("tmux 3.4", val).kitty_skip_reason(),
            None,
            "value {val:?} must not trigger skip",
        );
    }
}

#[test]
fn kitty_skip_old_tmux_takes_precedence_over_extended_keys() {
    assert_eq!(
        extended_keys_ctx("tmux 3.2", Some("off")).kitty_skip_reason(),
        Some("tmux_old"),
    );
}

#[test]
fn kitty_skip_vte_takes_precedence_over_tmux_extended_keys_off() {
    // VTE strips kitty CSI-u so `/terminal-setup` should recommend
    // switching terminal emulators rather than editing tmux.conf.
    let mut ctx = extended_keys_ctx("tmux 3.4", Some("off"));
    ctx.vte_version = Some("7402".to_owned());
    assert_eq!(ctx.kitty_skip_reason(), Some("vte"));
}

#[test]
fn kitty_skip_vte_takes_precedence_over_screen() {
    let ctx = TerminalContext {
        multiplexer: MultiplexerKind::Screen,
        vte_version: Some("7402".to_owned()),
        ..Default::default()
    };
    assert_eq!(ctx.kitty_skip_reason(), Some("vte"));
}

#[test]
fn kitty_skip_vte_takes_precedence_over_tmux_old() {
    let mut ctx = extended_keys_ctx("tmux 3.2", None);
    ctx.vte_version = Some("7402".to_owned());
    assert_eq!(ctx.kitty_skip_reason(), Some("vte"));
}

// =====================================================================
// JetBrains JediTerm detection
// =====================================================================

#[test]
fn brand_jetbrains_from_terminal_emulator() {
    let env = env_from(&[("TERMINAL_EMULATOR", "JetBrains-JediTerm")]);
    assert_eq!(
        detect_terminal_brand_from_env(&env),
        TerminalName::JetBrains
    );
}

#[test]
fn brand_jetbrains_case_insensitive() {
    let env = env_from(&[("TERMINAL_EMULATOR", "jetbrains-jediterm")]);
    assert_eq!(
        detect_terminal_brand_from_env(&env),
        TerminalName::JetBrains
    );
}

#[test]
fn brand_jetbrains_beats_term_session_id() {
    // JediTerm on some platforms may set TERM_SESSION_ID, which would
    // otherwise be detected as Apple Terminal. JetBrains must win.
    let env = env_from(&[
        ("TERMINAL_EMULATOR", "JetBrains-JediTerm"),
        ("TERM_SESSION_ID", "w0t0p0:ABC123"),
    ]);
    assert_eq!(
        detect_terminal_brand_from_env(&env),
        TerminalName::JetBrains
    );
}

#[test]
fn context_jetbrains_terminal() {
    let env = env_from(&[("TERMINAL_EMULATOR", "JetBrains-JediTerm")]);
    let ctx = build_terminal_context_from_env(&env);
    assert_eq!(ctx.brand, TerminalName::JetBrains);
    assert_eq!(ctx.multiplexer, MultiplexerKind::Undetected);
    assert!(!ctx.is_ssh);
}

#[test]
fn kitty_skip_jetbrains() {
    let ctx = TerminalContext {
        brand: TerminalName::JetBrains,
        ..Default::default()
    };
    assert_eq!(ctx.kitty_skip_reason(), Some("jetbrains"));
}

#[test]
fn ctrl_dot_unreliable_on_jetbrains() {
    let ctx = TerminalContext {
        brand: TerminalName::JetBrains,
        ..Default::default()
    };
    assert!(ctx.ctrl_dot_unreliable());
}

#[test]
fn repaints_pane_out_of_band_per_arm() {
    // Embedded-editor :terminal (libvterm) can repaint our pane with no PTY resize.
    let editor = TerminalContext {
        embedded_editor: Some(EmbeddedEditor::Neovim),
        multiplexer: MultiplexerKind::Undetected,
        ..Default::default()
    };
    assert!(editor.repaints_pane_out_of_band());

    // Each multiplexer arm fires independently.
    for mux in [
        MultiplexerKind::Tmux,
        MultiplexerKind::Screen,
        MultiplexerKind::Zellij,
    ] {
        let ctx = TerminalContext {
            multiplexer: mux,
            ..Default::default()
        };
        assert!(
            ctx.repaints_pane_out_of_band(),
            "multiplexer {mux:?} should report out-of-band repaint"
        );
    }

    // Plain terminal: no editor, no multiplexer -> no heal (no flicker regression).
    let plain = TerminalContext {
        embedded_editor: None,
        multiplexer: MultiplexerKind::Undetected,
        ..Default::default()
    };
    assert!(!plain.repaints_pane_out_of_band());
}
