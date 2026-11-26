//! Process-environment appearance hints when desktop APIs are unavailable.
//!
//! `LC_ECHO_AGENT_APPEARANCE` is the SSH-surviving alias (`AcceptEnv LC_*`).
//! `COLORFGBG` is a terminal polarity hint stamped once at shell start and
//! then inherited unchanged — a guess, not a live reading.
//!
//! Startup (`detect_with_osc11_fallback`): desktop → explicit wrap/SSH stamps
//! → OSC 11 → `COLORFGBG`. Runtime watcher (`detect`): desktop → explicit
//! stamps → cached startup OSC 11 → `COLORFGBG` (no new OSC 11 probe once
//! crossterm owns stdin).

use std::collections::HashMap;

use super::system_appearance::SystemAppearance;

/// Read appearance from the process environment (explicit hints + `COLORFGBG`).
///
/// Runtime watcher path. Startup uses [`detect_explicit_from_env_map`] then
/// OSC 11 then [`detect_colorfgbg_from_env_map`].
#[must_use]
pub fn detect() -> Option<SystemAppearance> {
    detect_from_env_map(&crate::host::collect_unicode_env())
}

/// Ordered lookup: `ECHO_AGENT_APPEARANCE`, then `LC_ECHO_AGENT_APPEARANCE`, then `COLORFGBG`.
#[must_use]
pub fn detect_from_env_map(env: &HashMap<String, String>) -> Option<SystemAppearance> {
    detect_explicit_from_env_map(env).or_else(|| detect_colorfgbg_from_env_map(env))
}

/// Deliberate wrap/SSH stamps only — no inherited `COLORFGBG` guess.
#[must_use]
pub fn detect_explicit_from_env_map(env: &HashMap<String, String>) -> Option<SystemAppearance> {
    parse_appearance_var(env_nonempty(env, "ECHO_AGENT_APPEARANCE"))
        .or_else(|| parse_appearance_var(env_nonempty(env, "LC_ECHO_AGENT_APPEARANCE")))
}

/// Inherited `COLORFGBG` polarity guess.
#[must_use]
pub fn detect_colorfgbg_from_env_map(env: &HashMap<String, String>) -> Option<SystemAppearance> {
    parse_colorfgbg(env_nonempty(env, "COLORFGBG"))
}

fn env_nonempty<'a>(env: &'a HashMap<String, String>, key: &str) -> Option<&'a str> {
    env.get(key)
        .map(String::as_str)
        .filter(|value| !value.is_empty())
}

/// Parse `dark`/`light` (plus `night`/`day` aliases). Unknown values are ignored.
#[must_use]
pub fn parse_appearance_var(raw: Option<&str>) -> Option<SystemAppearance> {
    match raw?.trim().to_ascii_lowercase().as_str() {
        "dark" | "night" => Some(SystemAppearance::Dark),
        "light" | "day" => Some(SystemAppearance::Light),
        _ => None,
    }
}

/// Parse `COLORFGBG`. Vim/Neovim heuristic: bg `0–6` and `8` are dark;
/// `7` and `9–15` are light. Non-ANSI indexes and a non-numeric last
/// field (`default`) yield `None`.
#[must_use]
pub fn parse_colorfgbg(raw: Option<&str>) -> Option<SystemAppearance> {
    // Last field is bg (`fg;bg` or `fg;default;bg`). A trailing `default`
    // means "unknown polarity", not "skip and use an earlier number".
    let bg = raw?.split(';').next_back()?.trim().parse::<u8>().ok()?;
    match bg {
        0..=6 | 8 => Some(SystemAppearance::Dark),
        7 | 9..=15 => Some(SystemAppearance::Light),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    #[test]
    fn echo_agent_appearance_dark_and_light() {
        assert_eq!(
            detect_from_env_map(&env(&[("ECHO_AGENT_APPEARANCE", "dark")])),
            Some(SystemAppearance::Dark)
        );
        assert_eq!(
            detect_from_env_map(&env(&[("ECHO_AGENT_APPEARANCE", "Light")])),
            Some(SystemAppearance::Light)
        );
        assert_eq!(
            detect_from_env_map(&env(&[("ECHO_AGENT_APPEARANCE", "night")])),
            Some(SystemAppearance::Dark)
        );
        assert_eq!(
            detect_from_env_map(&env(&[("ECHO_AGENT_APPEARANCE", "day")])),
            Some(SystemAppearance::Light)
        );
    }

    #[test]
    fn lc_alias_used_when_canonical_absent() {
        assert_eq!(
            detect_from_env_map(&env(&[("LC_ECHO_AGENT_APPEARANCE", "light")])),
            Some(SystemAppearance::Light)
        );
    }

    #[test]
    fn canonical_wins_over_lc_and_colorfgbg() {
        assert_eq!(
            detect_from_env_map(&env(&[
                ("ECHO_AGENT_APPEARANCE", "dark"),
                ("LC_ECHO_AGENT_APPEARANCE", "light"),
                ("COLORFGBG", "0;15"),
            ])),
            Some(SystemAppearance::Dark)
        );
    }

    #[test]
    fn unknown_or_empty_appearance_falls_through_to_colorfgbg() {
        assert_eq!(
            detect_from_env_map(&env(&[
                ("ECHO_AGENT_APPEARANCE", "solarized"),
                ("COLORFGBG", "15;0"),
            ])),
            Some(SystemAppearance::Dark)
        );
        assert_eq!(
            detect_from_env_map(&env(&[
                ("ECHO_AGENT_APPEARANCE", ""),
                ("COLORFGBG", "0;15"),
            ])),
            Some(SystemAppearance::Light)
        );
    }

    #[test]
    fn lc_wins_over_conflicting_colorfgbg() {
        assert_eq!(
            detect_from_env_map(&env(&[
                ("LC_ECHO_AGENT_APPEARANCE", "light"),
                ("COLORFGBG", "15;0"),
            ])),
            Some(SystemAppearance::Light)
        );
        assert_eq!(
            detect_from_env_map(&env(&[
                ("LC_ECHO_AGENT_APPEARANCE", "dark"),
                ("COLORFGBG", "0;15"),
            ])),
            Some(SystemAppearance::Dark)
        );
    }

    #[test]
    fn unknown_or_empty_lc_falls_through_to_colorfgbg_when_echo_agent_absent() {
        assert_eq!(
            detect_from_env_map(&env(&[
                ("LC_ECHO_AGENT_APPEARANCE", "solarized"),
                ("COLORFGBG", "15;0"),
            ])),
            Some(SystemAppearance::Dark)
        );
        assert_eq!(
            detect_from_env_map(&env(&[
                ("LC_ECHO_AGENT_APPEARANCE", ""),
                ("COLORFGBG", "0;15"),
            ])),
            Some(SystemAppearance::Light)
        );
    }

    #[test]
    fn colorfgbg_dark_and_light() {
        assert_eq!(parse_colorfgbg(Some("15;0")), Some(SystemAppearance::Dark));
        assert_eq!(parse_colorfgbg(Some("0;15")), Some(SystemAppearance::Light));
        assert_eq!(
            parse_colorfgbg(Some("15;default;0")),
            Some(SystemAppearance::Dark)
        );
        assert_eq!(
            parse_colorfgbg(Some("0;default;15")),
            Some(SystemAppearance::Light)
        );
        assert_eq!(parse_colorfgbg(Some("7;8")), Some(SystemAppearance::Dark));
        assert_eq!(parse_colorfgbg(Some("0;7")), Some(SystemAppearance::Light));
        assert_eq!(parse_colorfgbg(Some("0;6")), Some(SystemAppearance::Dark));
        assert_eq!(parse_colorfgbg(Some("0;9")), Some(SystemAppearance::Light));
    }

    #[test]
    fn colorfgbg_rejects_unknown() {
        assert_eq!(parse_colorfgbg(Some("15;default")), None);
        assert_eq!(parse_colorfgbg(Some("")), None);
        assert_eq!(parse_colorfgbg(None), None);
        assert_eq!(parse_colorfgbg(Some("1;99")), None);
    }

    #[test]
    fn empty_map_is_none() {
        assert_eq!(detect_from_env_map(&HashMap::new()), None);
    }

    #[test]
    fn explicit_hints_ignore_colorfgbg() {
        assert_eq!(
            detect_explicit_from_env_map(&env(&[("COLORFGBG", "15;0")])),
            None
        );
        assert_eq!(
            detect_explicit_from_env_map(&env(&[
                ("ECHO_AGENT_APPEARANCE", "light"),
                ("COLORFGBG", "15;0"),
            ])),
            Some(SystemAppearance::Light)
        );
    }

    #[test]
    fn colorfgbg_map_ignores_explicit_hints() {
        assert_eq!(
            detect_colorfgbg_from_env_map(&env(&[
                ("ECHO_AGENT_APPEARANCE", "light"),
                ("LC_ECHO_AGENT_APPEARANCE", "light"),
                ("COLORFGBG", "15;0"),
            ])),
            Some(SystemAppearance::Dark)
        );
        assert_eq!(
            detect_colorfgbg_from_env_map(&env(&[("ECHO_AGENT_APPEARANCE", "dark")])),
            None
        );
    }
}
