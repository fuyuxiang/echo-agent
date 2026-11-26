//! Terminal version capture from environment variables.
//!
//! **A version variable is trusted only when the environment corroborates the
//! brand it belongs to.** These variables cross process, SSH and multiplexer
//! boundaries, so an uncorroborated version is as likely to describe another
//! program as the terminal drawing our output. For a variable that is itself
//! the brand marker (`WEZTERM_VERSION`, `VTE_VERSION`), corroboration means no
//! stronger marker outranked it in `detect_terminal_brand_from_env`.
//!
//! Never read: `ZELLIJ_VERSION`, which would make an Alacritty pane inside
//! Zellij report Zellij's number as its own, and `KONSOLE_VERSION`, which has
//! no `TerminalName::Konsole` to attach to.

use std::collections::HashMap;

use super::{TerminalName, terminal_name_from_term_program};

/// Which source produced a [`TermVersion`].
///
/// The rendered labels are stable telemetry values — do not rename them.
#[derive(Clone, Copy, Debug, Eq, PartialEq, strum::Display)]
#[strum(serialize_all = "snake_case")]
pub enum TermVersionSource {
    None,
    /// The runtime [`crate::terminal::da2`] probe — the only non-env source.
    Da2,
    /// `TERM_PROGRAM_VERSION`, or its SSH-surviving `LC_TERMINAL_VERSION`
    /// mirror (iTerm2 only).
    TermProgram,
    #[strum(serialize = "wezterm")]
    WezTerm,
    Vte,
}

/// A terminal version together with the source that reported it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TermVersion {
    /// Raw, exactly as the source reported it (trimmed). Shapes vary by
    /// terminal: `"3.5.6"`, `"20240203-110809-5046fc22"`, `"7402"`.
    pub version: String,
    pub source: TermVersionSource,
}

impl TermVersion {
    fn new(version: &str, source: TermVersionSource) -> Self {
        Self {
            version: version.to_owned(),
            source,
        }
    }
}

/// Whether the brand `TERM_PROGRAM` names vouches for `env_brand`'s version.
///
/// Identity, widened for VS Code forks: they export `TERM_PROGRAM=vscode` from
/// the same host process that writes their brand marker and draws our output,
/// so the version is that host's and `brand` records whose numbering it is.
/// One-directional, so a leaked marker cannot borrow another brand's version;
/// Zed is excluded as it is not an xterm.js host.
fn corroborates(named: TerminalName, env_brand: TerminalName) -> bool {
    named == env_brand
        || (named == TerminalName::VsCode
            && matches!(
                env_brand,
                TerminalName::VsCode | TerminalName::Cursor | TerminalName::Windsurf
            ))
}

/// Pick the best available version: a runtime probe outranks the environment,
/// since a live self-report cannot be inherited across a process, SSH or
/// multiplexer boundary, nor go stale. XTVERSION has no arm — its payload is a
/// name-and-version string, and it rides `TerminalTelemetry::xtversion`.
pub(super) fn best_term_version(
    da2: Option<&str>,
    env_version: Option<&TermVersion>,
) -> (String, TermVersionSource) {
    da2.map(|version| (version.to_owned(), TermVersionSource::Da2))
        .or_else(|| env_version.map(|v| (v.version.clone(), v.source)))
        .unwrap_or_else(|| (String::new(), TermVersionSource::None))
}

/// Look up an env value, trimmed; `env_get` alone would pass whitespace.
fn env_trimmed<'a>(env: &'a HashMap<String, String>, key: &str) -> Option<&'a str> {
    let value = super::env_get(env, key)?.trim();
    (!value.is_empty()).then_some(value)
}

/// Resolve the terminal version from the environment, taking the first
/// variable corroborated by `env_brand`.
///
/// `env_brand` is the *pre-refinement* brand, before
/// `refine_unknown_brand_for_host` may rewrite `TerminalContext::brand`:
/// the native-Windows `Unknown -> WindowsTerminal` guess must not license a
/// version attribution.
pub(super) fn detect_env_term_version(
    env: &HashMap<String, String>,
    env_brand: TerminalName,
) -> Option<TermVersion> {
    // tmux >= 3.2 exports TERM_PROGRAM=tmux and its own TERM_PROGRAM_VERSION,
    // which ungated would land on whichever brand marker survived inside the
    // tmux server environment.
    let named_brand = env_trimmed(env, "TERM_PROGRAM").and_then(terminal_name_from_term_program);
    if let Some(version) = env_trimmed(env, "TERM_PROGRAM_VERSION")
        && named_brand.is_some_and(|named| corroborates(named, env_brand))
    {
        return Some(TermVersion::new(version, TermVersionSource::TermProgram));
    }

    // LC_TERMINAL_VERSION survives SSH where TERM_PROGRAM_VERSION does not,
    // but only iTerm2 sets the pair.
    if let Some(version) = env_trimmed(env, "LC_TERMINAL_VERSION")
        && env_trimmed(env, "LC_TERMINAL").is_some_and(|v| v.eq_ignore_ascii_case("iterm2"))
        && env_brand == TerminalName::Iterm2
    {
        return Some(TermVersion::new(version, TermVersionSource::TermProgram));
    }

    if let Some(version) = env_trimmed(env, "WEZTERM_VERSION")
        && env_brand == TerminalName::WezTerm
    {
        return Some(TermVersion::new(version, TermVersionSource::WezTerm));
    }

    // Brand-only: `TerminalContext::is_vte_based()` also accepts a present
    // `vte_version`, which is the candidate here — routing the gate through it
    // would make it vacuous.
    if let Some(version) = env_trimmed(env, "VTE_VERSION")
        && env_brand.is_vte_based()
    {
        return Some(TermVersion::new(version, TermVersionSource::Vte));
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::{build_terminal_context_from_env, env_from};

    fn resolved(pairs: &[(&str, &str)]) -> (String, TermVersionSource) {
        build_terminal_context_from_env(&env_from(pairs)).term_version()
    }

    /// Unlike the others, this reads ambient host state and warms process-wide
    /// caches; the asserted fields still come only from the injected map.
    fn snapshot(pairs: &[(&str, &str)]) -> echo_agent_telemetry::events::TerminalTelemetry {
        build_terminal_context_from_env(&env_from(pairs)).telemetry_snapshot()
    }

    #[test]
    fn source_labels_are_pinned() {
        assert_eq!(TermVersionSource::None.to_string(), "none");
        assert_eq!(TermVersionSource::Da2.to_string(), "da2");
        assert_eq!(TermVersionSource::TermProgram.to_string(), "term_program");
        assert_eq!(TermVersionSource::WezTerm.to_string(), "wezterm");
        assert_eq!(TermVersionSource::Vte.to_string(), "vte");
    }

    /// Driven through `best_term_version` rather than the probe's process-global
    /// `OnceLock`: this crate's tests share one process, so recording a reply
    /// would race every env-precedence assertion below.
    #[test]
    fn a_probed_version_outranks_env() {
        let env = TermVersion::new("7402", TermVersionSource::Vte);
        assert_eq!(
            best_term_version(Some("0.25.0"), Some(&env)),
            ("0.25.0".to_owned(), TermVersionSource::Da2)
        );
        assert_eq!(
            best_term_version(None, Some(&env)),
            ("7402".to_owned(), TermVersionSource::Vte)
        );
        assert_eq!(
            best_term_version(None, None),
            (String::new(), TermVersionSource::None)
        );
    }

    /// The version has to reach the feedback card; its source has no field on
    /// the wire type to reach it through.
    #[test]
    fn feedback_info_carries_the_version() {
        let present =
            build_terminal_context_from_env(&env_from(&[("VTE_VERSION", "7402")])).feedback_info();
        assert_eq!(present.term_version.as_deref(), Some("7402"));

        let absent = build_terminal_context_from_env(&env_from(&[("TERM", "xterm-256color")]))
            .feedback_info();
        assert_eq!(absent.term_version, None);
    }

    #[test]
    fn term_program_version_wins_when_term_program_names_the_brand() {
        let (version, source) = resolved(&[
            ("TERM_PROGRAM", "iTerm.app"),
            ("TERM_PROGRAM_VERSION", "3.5.6"),
            ("LC_TERMINAL", "iTerm2"),
            ("LC_TERMINAL_VERSION", "3.4.0"),
        ]);
        assert_eq!(version, "3.5.6");
        assert_eq!(source, TermVersionSource::TermProgram);
    }

    #[test]
    fn term_program_version_wins_over_wezterm_version() {
        let (version, source) = resolved(&[
            ("TERM_PROGRAM", "WezTerm"),
            ("TERM_PROGRAM_VERSION", "20240203-110809-5046fc22"),
            ("WEZTERM_VERSION", "20230712-072601-f4abf8fd"),
        ]);
        assert_eq!(version, "20240203-110809-5046fc22");
        assert_eq!(source, TermVersionSource::TermProgram);
    }

    #[test]
    fn vscode_and_its_forks_keep_the_vscode_host_version() {
        let (version, source) = resolved(&[
            ("VSCODE_GIT_ASKPASS_MAIN", "/home/u/.vscode-server/askpass"),
            ("TERM_PROGRAM", "vscode"),
            ("TERM_PROGRAM_VERSION", "1.99.3"),
        ]);
        assert_eq!(version, "1.99.3");
        assert_eq!(source, TermVersionSource::TermProgram);

        let fork = build_terminal_context_from_env(&env_from(&[
            ("CURSOR_TRACE_ID", "abc123"),
            ("TERM_PROGRAM", "vscode"),
            ("TERM_PROGRAM_VERSION", "1.99.3"),
        ]));
        assert_eq!(fork.brand, TerminalName::Cursor);
        let (version, source) = fork.term_version();
        assert_eq!(version, "1.99.3");
        assert_eq!(source, TermVersionSource::TermProgram);
    }

    #[test]
    fn the_vscode_widening_is_one_directional() {
        // TERM_PROGRAM names Zed while the brand chain resolves Cursor —
        // neither vouches for the other.
        let (version, source) = resolved(&[
            ("CURSOR_TRACE_ID", "abc123"),
            ("TERM_PROGRAM", "zed"),
            ("TERM_PROGRAM_VERSION", "0.180.0"),
        ]);
        assert_eq!(version, "");
        assert_eq!(source, TermVersionSource::None);
    }

    #[test]
    fn lc_terminal_version_wins_when_term_program_version_is_absent() {
        let (version, source) =
            resolved(&[("LC_TERMINAL", "iTerm2"), ("LC_TERMINAL_VERSION", "3.5.6")]);
        assert_eq!(version, "3.5.6");
        assert_eq!(source, TermVersionSource::TermProgram);
    }

    #[test]
    fn lc_terminal_version_ignored_without_lc_terminal() {
        let (version, source) = resolved(&[
            ("ITERM_SESSION_ID", "w0t0p0:1234"),
            ("LC_TERMINAL_VERSION", "3.5.6"),
        ]);
        assert_eq!(version, "");
        assert_eq!(source, TermVersionSource::None);
    }

    #[test]
    fn wezterm_version_wins_over_vte_version() {
        let (version, source) = resolved(&[
            ("WEZTERM_VERSION", "20240203-110809-5046fc22"),
            ("VTE_VERSION", "7402"),
        ]);
        assert_eq!(version, "20240203-110809-5046fc22");
        assert_eq!(source, TermVersionSource::WezTerm);
    }

    #[test]
    fn vte_version_wins_for_a_vte_brand() {
        let (version, source) = resolved(&[("VTE_VERSION", "7402")]);
        assert_eq!(version, "7402");
        assert_eq!(source, TermVersionSource::Vte);
    }

    #[test]
    fn wezterm_version_ignored_for_another_brand() {
        // An inherited WEZTERM_VERSION is not the Ghostty session's version.
        let (version, source) = resolved(&[
            ("TERM_PROGRAM", "Ghostty"),
            ("WEZTERM_VERSION", "20240203-110809-5046fc22"),
        ]);
        assert_eq!(version, "");
        assert_eq!(source, TermVersionSource::None);
    }

    #[test]
    fn vte_version_ignored_for_a_non_vte_brand() {
        let (version, source) = resolved(&[("TERM", "alacritty"), ("VTE_VERSION", "7402")]);
        assert_eq!(version, "");
        assert_eq!(source, TermVersionSource::None);
    }

    #[test]
    fn tmux_term_program_version_is_not_the_terminal_version() {
        // tmux >= 3.2 exports TERM_PROGRAM=tmux plus its own version, and
        // iTerm2's releases are also 3.5.x — an ungated value would be
        // indistinguishable from a real one.
        let (version, source) = resolved(&[
            ("TMUX", "/tmp/tmux-501/default,12345,0"),
            ("TERM_PROGRAM", "tmux"),
            ("TERM_PROGRAM_VERSION", "3.5"),
            ("ITERM_SESSION_ID", "w0t0p0:1234"),
        ]);
        assert_eq!(version, "");
        assert_eq!(source, TermVersionSource::None);
    }

    #[test]
    fn iterm2_in_tmux_falls_through_to_lc_terminal_version() {
        let (version, source) = resolved(&[
            ("TMUX", "/tmp/tmux-501/default,12345,0"),
            ("TERM_PROGRAM", "tmux"),
            ("TERM_PROGRAM_VERSION", "3.5"),
            ("LC_TERMINAL", "iTerm2"),
            ("LC_TERMINAL_VERSION", "3.5.6"),
        ]);
        assert_eq!(version, "3.5.6");
        assert_eq!(source, TermVersionSource::TermProgram);
    }

    #[test]
    fn blank_version_is_absent() {
        let (version, source) = resolved(&[
            ("TERM_PROGRAM", "Ghostty"),
            ("TERM_PROGRAM_VERSION", "   \t "),
        ]);
        assert_eq!(version, "");
        assert_eq!(source, TermVersionSource::None);
    }

    #[test]
    fn surrounding_whitespace_is_trimmed() {
        let (version, source) = resolved(&[
            ("TERM_PROGRAM", "Ghostty"),
            ("TERM_PROGRAM_VERSION", " 1.1.3\n"),
        ]);
        assert_eq!(version, "1.1.3");
        assert_eq!(source, TermVersionSource::TermProgram);
    }

    #[test]
    fn telemetry_snapshot_carries_version_and_source() {
        let populated = snapshot(&[("VTE_VERSION", "7402")]);
        assert_eq!(populated.term_version, "7402");
        assert_eq!(populated.term_version_source, "vte");

        let empty = snapshot(&[("TERM", "xterm-256color")]);
        assert_eq!(empty.term_version, "");
        assert_eq!(empty.term_version_source, "none");
    }
}
