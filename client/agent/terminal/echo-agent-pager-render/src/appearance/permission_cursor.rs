//! Permission-prompt cursor preselection.
//!
//! Single home for everything that decides which row the approval menu
//! highlights when the agent asks for permission:
//!
//! - [`DefaultSelectedPermission`] — the value type (config + ACP-kind bridge),
//! - the process-wide caches (configured value + sticky last-used),
//! - [`resolve_initial_cursor`] — the one function the ACP handler calls when
//!   queueing a prompt.
//!
//! Deliberately kept out of `views::permission_view` (a renderer) and out of
//! `appearance::cache` (generic bool/u8 setting caches) so this cross-cutting
//! type and its state live together, and the render-hot-path cache module
//! doesn't have to depend upward on the view layer.

use std::cell::Cell;

use agent_client_protocol as acp;
use echo_agent_workspace::permission::is_enable_always_approve_option;

/// Which row the approval-menu cursor preselects (the highlighted row).
///
/// Persisted as `[ui].default_selected_permission`. The four variants map onto
/// the rows a permission prompt can show:
///
/// - [`AlwaysAllowAllSessions`](Self::AlwaysAllowAllSessions) — the global
///   "enable always-approve" row ("Always allow on all sessions"). This is
///   also the value used when the setting is unset or unrecognised, so it is
///   the effective default.
/// - [`AllowOnce`](Self::AllowOnce) — the plain "Yes" / allow-once row.
/// - [`AllowCommandAlways`](Self::AllowCommandAlways) — the prompt-scoped
///   always-allow row ("Always allow this command" — also covers the
///   per-tool / per-domain / per-edit-session variants of the same ACP kind).
/// - [`Reject`](Self::Reject) — the reject row.
///
/// The configured value only steers the **first** prompt of a session; after
/// the user confirms a prompt, [`resolve_initial_cursor`] sticks to the
/// last-used kind (recorded via [`set_last_used_permission`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DefaultSelectedPermission {
    /// The global "Always allow on all sessions" (enable-always-approve) row.
    /// Also the fallback for an unset / unrecognised config value, so it is the
    /// effective default.
    AlwaysAllowAllSessions,
    AllowOnce,
    /// The prompt-scoped always-allow row ("Always allow this command" /
    /// tool / domain / edit-session).
    AllowCommandAlways,
    Reject,
}

impl DefaultSelectedPermission {
    /// The single canonical config.toml / registry string for this variant.
    /// One accepted value per variant — there are no aliases. `const` so the
    /// settings catalog can build its choice table at compile time.
    pub const fn as_canonical(self) -> &'static str {
        match self {
            Self::AlwaysAllowAllSessions => "always_allow_all_sessions",
            Self::AllowOnce => "allow_once",
            Self::AllowCommandAlways => "allow_command_always",
            Self::Reject => "reject",
        }
    }

    /// Display label for the settings picker and the change toast.
    /// `AllowCommandAlways` preselects the prompt-specific always-allow row
    /// (per-command / per-tool / per-domain / per-edit-session), never a
    /// global allow-everything — that is `AlwaysAllowAllSessions`.
    pub const fn display(self) -> &'static str {
        match self {
            Self::AlwaysAllowAllSessions => "Always allow on all sessions",
            Self::AllowOnce => "Allow once",
            Self::AllowCommandAlways => "Always allow this command",
            Self::Reject => "Reject",
        }
    }

    /// Parse a config.toml / registry value (trimmed, case-insensitive, no
    /// aliases). The mapping is total — `always_allow_all_sessions` and any
    /// unrecognised / empty value both resolve to
    /// [`AlwaysAllowAllSessions`](Self::AlwaysAllowAllSessions), so no `Option`
    /// has to be threaded through callers.
    pub fn from_config_value(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "allow_once" => Self::AllowOnce,
            "allow_command_always" => Self::AllowCommandAlways,
            "reject" => Self::Reject,
            _ => Self::AlwaysAllowAllSessions,
        }
    }

    /// Whether this preselection targets the given ACP option kind.
    ///
    /// [`AlwaysAllowAllSessions`](Self::AlwaysAllowAllSessions) targets no
    /// specific kind — its row is the enable-always-approve option, matched by
    /// identity in [`resolve_initial_cursor`], so the cursor falls there when
    /// nothing else matches. [`Reject`](Self::Reject) matches **both** reject
    /// kinds so a sticky reject lands on whichever reject row a given prompt
    /// offers (some prompts carry only `RejectAlways`).
    pub fn matches_kind(self, kind: &acp::PermissionOptionKind) -> bool {
        matches!(
            (self, kind),
            (Self::AllowOnce, acp::PermissionOptionKind::AllowOnce)
                | (
                    Self::AllowCommandAlways,
                    acp::PermissionOptionKind::AllowAlways
                )
                | (
                    Self::Reject,
                    acp::PermissionOptionKind::RejectOnce | acp::PermissionOptionKind::RejectAlways
                )
        )
    }

    /// Map a confirmed ACP option kind onto the sticky "last used" target.
    /// Both reject kinds collapse to [`Reject`](Self::Reject). Total over every
    /// ACP kind, so it never yields
    /// [`AlwaysAllowAllSessions`](Self::AlwaysAllowAllSessions) — that row is
    /// the enable-always-approve option, which callers exclude from sticky
    /// recording.
    pub fn from_kind(kind: &acp::PermissionOptionKind) -> Self {
        match kind {
            acp::PermissionOptionKind::AllowOnce => Self::AllowOnce,
            acp::PermissionOptionKind::AllowAlways => Self::AllowCommandAlways,
            acp::PermissionOptionKind::RejectOnce | acp::PermissionOptionKind::RejectAlways => {
                Self::Reject
            }
            // TODO(acp-0.10): `PermissionOptionKind` is #[non_exhaustive];
            // treat unknown kinds as reject (never auto-allow).
            _ => Self::Reject,
        }
    }
}

// ── Configured value cache: `[ui].default_selected_permission` ──────────────
//
// Read when queueing the first prompt of a session. Seeded by `prime` at
// startup (and lazily on first read) so the path never hits disk mid-session.
// `AlwaysAllowAllSessions` represents the effective default (unset).

thread_local! {
    static CONFIG_CURRENT: Cell<DefaultSelectedPermission> =
        const { Cell::new(DefaultSelectedPermission::AlwaysAllowAllSessions) };
    static CONFIG_LOADED: Cell<bool> = const { Cell::new(false) };
}

/// Read the cached `[ui].default_selected_permission`, seeding on first call.
///
/// Precedence (mirrors `appearance::cache::load_scroll_speed`):
///
/// 1. `ECHO_AGENT_DEFAULT_SELECTED_PERMISSION` env var (headless / agent testing —
///    overrides `config.toml` without editing it),
/// 2. `[ui].default_selected_permission` in the layered effective config,
/// 3. [`AlwaysAllowAllSessions`](DefaultSelectedPermission::AlwaysAllowAllSessions)
///    (the effective default).
///
/// Unrecognised / empty values at any layer fall through to the next.
pub fn load_default_selected_permission() -> DefaultSelectedPermission {
    CONFIG_LOADED.with(|loaded| {
        if !loaded.get() {
            let resolved = std::env::var("ECHO_AGENT_DEFAULT_SELECTED_PERMISSION")
                .ok()
                .map(|s| DefaultSelectedPermission::from_config_value(&s))
                .filter(|p| *p != DefaultSelectedPermission::AlwaysAllowAllSessions)
                .or_else(|| {
                    load_string_from_effective_config("default_selected_permission")
                        .map(|s| DefaultSelectedPermission::from_config_value(&s))
                })
                .unwrap_or(DefaultSelectedPermission::AlwaysAllowAllSessions);
            CONFIG_CURRENT.with(|c| c.set(resolved));
            loaded.set(true);
        }
    });
    CONFIG_CURRENT.with(Cell::get)
}

/// Replace the cached configured value (optimistic update from the settings
/// modal, or rollback on persist failure). The next prompt sees it without a
/// restart.
pub fn set_default_selected_permission(value: DefaultSelectedPermission) {
    CONFIG_CURRENT.with(|c| c.set(value));
    CONFIG_LOADED.with(|l| l.set(true));
}

/// Eagerly seed the configured-value cache at startup (called by
/// `appearance::cache::prime`) so the first prompt never hits disk.
pub fn prime() {
    let _ = load_default_selected_permission();
}

// ── Sticky "last used" cursor target ────────────────────────────────────────
//
// Process-wide ephemeral state: the kind the user most recently confirmed.
// After the first prompt, `resolve_initial_cursor` prefers this over the
// configured value. `AlwaysAllowAllSessions` is the sentinel meaning nothing
// has been confirmed yet (`from_kind` never produces it). The TUI renders +
// dispatches on a single thread, so a thread-local `Cell` is fine.

thread_local! {
    static LAST_USED: Cell<DefaultSelectedPermission> =
        const { Cell::new(DefaultSelectedPermission::AlwaysAllowAllSessions) };
}

/// The kind the user last confirmed this session, or the
/// [`AlwaysAllowAllSessions`](DefaultSelectedPermission::AlwaysAllowAllSessions)
/// sentinel if none yet.
pub fn last_used_permission() -> DefaultSelectedPermission {
    LAST_USED.with(Cell::get)
}

/// Record the kind the user just confirmed. Callers must skip the special
/// enable-always-approve (YOLO) and allow-edits-session options — neither
/// represents a per-prompt choice that should steer a later prompt's cursor.
pub fn set_last_used_permission(kind: DefaultSelectedPermission) {
    LAST_USED.with(|c| c.set(kind));
}

// ── Resolution ──────────────────────────────────────────────────────────────

/// Pick the initially-highlighted row for a freshly-queued permission prompt.
///
/// Precedence:
///
/// 1. the sticky last-used kind (once the user has confirmed any prompt),
/// 2. the configured `[ui].default_selected_permission`,
/// 3. the global "Always allow on all sessions" (enable-always-approve) row,
///    matched by identity via `is_enable_always_approve_option` — not by list
///    position, so the intent lives in the code rather than the option order,
/// 4. index 0 (clients that don't get the YOLO row prepended).
///
/// The YOLO row is skipped while a concrete target kind is in play, so a
/// configured / sticky preselection never lands on it — including when the
/// target kind has no matching row (the always-allow row may be suppressed
/// as unhonorable): a concrete target degrades to the one-shot allow row,
/// never escalates to the global always-approve row.
pub fn resolve_initial_cursor(options: &[acp::PermissionOption]) -> usize {
    let target = match last_used_permission() {
        DefaultSelectedPermission::AlwaysAllowAllSessions => load_default_selected_permission(),
        sticky => sticky,
    };
    if let Some(idx) = options
        .iter()
        .position(|o| target.matches_kind(&o.kind) && !is_enable_always_approve_option(o))
    {
        return idx;
    }
    if target != DefaultSelectedPermission::AlwaysAllowAllSessions
        && let Some(idx) = options.iter().position(|o| {
            o.kind == acp::PermissionOptionKind::AllowOnce && !is_enable_always_approve_option(o)
        })
    {
        return idx;
    }
    options
        .iter()
        .position(is_enable_always_approve_option)
        .unwrap_or(0)
}

/// Read a `[ui].<key>` string from the shell's layered effective config.
/// Returns `None` when the key is absent or not a string.
fn load_string_from_effective_config(key: &str) -> Option<String> {
    let root = echo_agent_config::load_effective_config_disk_only().ok()?;
    root.get("ui")?
        .get(key)?
        .as_str()
        .map(std::string::ToString::to_string)
}

// -- Tests -------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use echo_agent_workspace::permission::ENABLE_ALWAYS_APPROVE_OPTION_ID;

    fn opt(id: &str, kind: acp::PermissionOptionKind) -> acp::PermissionOption {
        acp::PermissionOption::new(acp::PermissionOptionId::new(id), id.to_owned(), kind)
    }

    #[test]
    fn from_config_value_accepts_one_canonical_per_variant() {
        // Trimmed + case-insensitive, but still the exact canonical token.
        assert_eq!(
            DefaultSelectedPermission::from_config_value("allow_once"),
            DefaultSelectedPermission::AllowOnce
        );
        assert_eq!(
            DefaultSelectedPermission::from_config_value("  ALLOW_COMMAND_ALWAYS  "),
            DefaultSelectedPermission::AllowCommandAlways
        );
        assert_eq!(
            DefaultSelectedPermission::from_config_value("reject"),
            DefaultSelectedPermission::Reject
        );
        assert_eq!(
            DefaultSelectedPermission::from_config_value("always_allow_all_sessions"),
            DefaultSelectedPermission::AlwaysAllowAllSessions
        );
        // Empty and garbage collapse to the effective default.
        assert_eq!(
            DefaultSelectedPermission::from_config_value(""),
            DefaultSelectedPermission::AlwaysAllowAllSessions
        );
        assert_eq!(
            DefaultSelectedPermission::from_config_value("bogus"),
            DefaultSelectedPermission::AlwaysAllowAllSessions
        );
    }

    #[test]
    fn canonical_round_trips() {
        // `as_canonical` is the inverse of `from_config_value` for every
        // variant — the enum is the single source of truth for the strings.
        for variant in [
            DefaultSelectedPermission::AlwaysAllowAllSessions,
            DefaultSelectedPermission::AllowOnce,
            DefaultSelectedPermission::AllowCommandAlways,
            DefaultSelectedPermission::Reject,
        ] {
            assert_eq!(
                DefaultSelectedPermission::from_config_value(variant.as_canonical()),
                variant,
            );
            assert!(!variant.display().is_empty());
        }
    }

    #[test]
    fn matches_kind_targets_the_right_rows() {
        use DefaultSelectedPermission as P;
        assert!(P::AllowOnce.matches_kind(&acp::PermissionOptionKind::AllowOnce));
        assert!(P::AllowCommandAlways.matches_kind(&acp::PermissionOptionKind::AllowAlways));
        assert!(!P::AllowCommandAlways.matches_kind(&acp::PermissionOptionKind::AllowOnce));
        // A sticky reject must land on EITHER reject row, since some prompts
        // only offer `RejectAlways`.
        assert!(P::Reject.matches_kind(&acp::PermissionOptionKind::RejectOnce));
        assert!(P::Reject.matches_kind(&acp::PermissionOptionKind::RejectAlways));
        // The always-allow-on-all-sessions sentinel targets no specific kind
        // (its row is matched by identity; the cursor falls to it).
        for kind in [
            acp::PermissionOptionKind::AllowOnce,
            acp::PermissionOptionKind::AllowAlways,
            acp::PermissionOptionKind::RejectOnce,
            acp::PermissionOptionKind::RejectAlways,
        ] {
            assert!(!P::AlwaysAllowAllSessions.matches_kind(&kind));
        }
    }

    #[test]
    fn from_kind_is_total_and_collapses_reject() {
        assert_eq!(
            DefaultSelectedPermission::from_kind(&acp::PermissionOptionKind::AllowOnce),
            DefaultSelectedPermission::AllowOnce
        );
        assert_eq!(
            DefaultSelectedPermission::from_kind(&acp::PermissionOptionKind::AllowAlways),
            DefaultSelectedPermission::AllowCommandAlways
        );
        assert_eq!(
            DefaultSelectedPermission::from_kind(&acp::PermissionOptionKind::RejectOnce),
            DefaultSelectedPermission::Reject
        );
        // `RejectAlways` ("No, and don't run X") collapses to Reject so the
        // next prompt still lands on its reject row.
        assert_eq!(
            DefaultSelectedPermission::from_kind(&acp::PermissionOptionKind::RejectAlways),
            DefaultSelectedPermission::Reject
        );
    }

    #[test]
    fn last_used_round_trips() {
        // Fresh thread = un-seeded thread-local (starts at the sentinel).
        std::thread::spawn(|| {
            assert_eq!(
                last_used_permission(),
                DefaultSelectedPermission::AlwaysAllowAllSessions
            );
            set_last_used_permission(DefaultSelectedPermission::AllowCommandAlways);
            assert_eq!(
                last_used_permission(),
                DefaultSelectedPermission::AllowCommandAlways
            );
            set_last_used_permission(DefaultSelectedPermission::Reject);
            assert_eq!(last_used_permission(), DefaultSelectedPermission::Reject);
        })
        .join()
        .unwrap();
    }

    #[test]
    fn resolve_unset_lands_on_yolo_row() {
        std::thread::spawn(|| {
            // Force the config cache to the default without touching disk/env.
            set_default_selected_permission(DefaultSelectedPermission::AlwaysAllowAllSessions);
            let options = [
                opt("allow-once", acp::PermissionOptionKind::AllowOnce),
                opt(
                    ENABLE_ALWAYS_APPROVE_OPTION_ID,
                    acp::PermissionOptionKind::AllowOnce,
                ),
                opt("reject-once", acp::PermissionOptionKind::RejectOnce),
            ];
            // No sticky + default config → the enable-always-approve row,
            // matched by identity (index 1), not the first AllowOnce (index 0).
            assert_eq!(resolve_initial_cursor(&options), 1);
        })
        .join()
        .unwrap();
    }

    #[test]
    fn resolve_sticky_skips_yolo_row() {
        std::thread::spawn(|| {
            set_default_selected_permission(DefaultSelectedPermission::AlwaysAllowAllSessions);
            set_last_used_permission(DefaultSelectedPermission::AllowOnce);
            let options = [
                opt(
                    ENABLE_ALWAYS_APPROVE_OPTION_ID,
                    acp::PermissionOptionKind::AllowOnce,
                ),
                opt("allow-once", acp::PermissionOptionKind::AllowOnce),
                opt("reject-once", acp::PermissionOptionKind::RejectOnce),
            ];
            // Sticky AllowOnce must skip the YOLO row (also AllowOnce kind) and
            // land on the plain allow-once row (index 1).
            assert_eq!(resolve_initial_cursor(&options), 1);
        })
        .join()
        .unwrap();
    }

    #[test]
    fn resolve_sticky_reject_lands_on_reject_always_only_prompt() {
        std::thread::spawn(|| {
            set_default_selected_permission(DefaultSelectedPermission::AlwaysAllowAllSessions);
            set_last_used_permission(DefaultSelectedPermission::Reject);
            let options = [
                opt(
                    ENABLE_ALWAYS_APPROVE_OPTION_ID,
                    acp::PermissionOptionKind::AllowOnce,
                ),
                opt("allow-once", acp::PermissionOptionKind::AllowOnce),
                opt("reject-always", acp::PermissionOptionKind::RejectAlways),
            ];
            // The prompt offers only `RejectAlways`; a sticky reject must still
            // find it (index 2) rather than falling back to the YOLO row.
            assert_eq!(resolve_initial_cursor(&options), 2);
        })
        .join()
        .unwrap();
    }

    #[test]
    fn resolve_without_yolo_row_falls_back_to_index_0() {
        std::thread::spawn(|| {
            set_default_selected_permission(DefaultSelectedPermission::AlwaysAllowAllSessions);
            let options = [
                opt("allow-once", acp::PermissionOptionKind::AllowOnce),
                opt("reject-once", acp::PermissionOptionKind::RejectOnce),
            ];
            // No sticky, default config, no YOLO row (non-TUI client) → index 0.
            assert_eq!(resolve_initial_cursor(&options), 0);
        })
        .join()
        .unwrap();
    }

    /// A concrete sticky target with no matching row (the always-allow row
    /// may be suppressed as unhonorable) degrades to the one-shot allow —
    /// never to the global always-approve row, which is one Enter away from
    /// persisting always-approve mode.
    #[test]
    fn unmatched_concrete_target_degrades_to_allow_once_not_yolo() {
        std::thread::spawn(|| {
            set_last_used_permission(DefaultSelectedPermission::AllowCommandAlways);
            let options = [
                opt(
                    echo_agent_workspace::permission::ENABLE_ALWAYS_APPROVE_OPTION_ID,
                    acp::PermissionOptionKind::AllowOnce,
                ),
                opt("allow-once", acp::PermissionOptionKind::AllowOnce),
                opt("reject-once", acp::PermissionOptionKind::RejectOnce),
                opt(
                    "reject-always-command",
                    acp::PermissionOptionKind::RejectAlways,
                ),
            ];
            assert_eq!(
                resolve_initial_cursor(&options),
                1,
                "cursor must land on the plain allow-once row"
            );
        })
        .join()
        .unwrap();
    }
}
