//! Vendor compatibility configuration for third-party agent surfaces
//! (skills, rules, agents, MCPs, hooks, sessions).
//!
//! Historically the agent hard-coded the dir lists `[".echo-agent", ".agents",
//! ".claude", ".cursor"]` (and `RULES_DIRS` / `AGENT_FILENAMES`) across ~6
//! call sites in three crates. This module now owns the canonical cell registry
//! used by runtime resolution and diagnostics (env var → config TOML → remote
//! setting → default ON).
//!
//! Two forms:
//! - [`CompatConfigToml`] — raw, parsed from the `[compat]` TOML section. Each
//!   cell is `Option<bool>` so `None` falls through to the resolution chain.
//! - [`CompatConfig`] — resolved plain bools consumed at runtime. Every cell
//!   defaults on.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompatVendor {
    Cursor,
    Claude,
    Codex,
}

impl CompatVendor {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Cursor => "cursor",
            Self::Claude => "claude",
            Self::Codex => "codex",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompatSurface {
    Skills,
    Rules,
    Agents,
    Mcps,
    Hooks,
    Sessions,
}

impl CompatSurface {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Skills => "skills",
            Self::Rules => "rules",
            Self::Agents => "agents",
            Self::Mcps => "mcps",
            Self::Hooks => "hooks",
            Self::Sessions => "sessions",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompatRemoteKey {
    CursorSkills,
    CursorRules,
    CursorAgents,
    CursorMcps,
    CursorHooks,
    CursorSessions,
    ClaudeSkills,
    ClaudeRules,
    ClaudeAgents,
    ClaudeMcps,
    ClaudeHooks,
    ClaudeSessions,
    CodexSessions,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompatCell {
    vendor: CompatVendor,
    surface: CompatSurface,
    env_var: &'static str,
    remote_key: Option<CompatRemoteKey>,
}

impl CompatCell {
    const fn new(
        vendor: CompatVendor,
        surface: CompatSurface,
        env_var: &'static str,
        remote_key: Option<CompatRemoteKey>,
    ) -> Self {
        Self {
            vendor,
            surface,
            env_var,
            remote_key,
        }
    }

    pub const fn vendor(self) -> CompatVendor {
        self.vendor
    }

    pub const fn surface(self) -> CompatSurface {
        self.surface
    }

    pub const fn env_var(self) -> &'static str {
        self.env_var
    }

    pub const fn remote_key(self) -> Option<CompatRemoteKey> {
        self.remote_key
    }

    /// Whether EchoAgent currently implements this compatibility surface.
    ///
    /// Codex non-session cells remain reserved in the registry so their config
    /// shape is stable, but runtime discovery does not consume them.
    pub const fn is_runtime_supported(self) -> bool {
        match self.vendor {
            CompatVendor::Cursor | CompatVendor::Claude => true,
            CompatVendor::Codex => matches!(self.surface, CompatSurface::Sessions),
        }
    }
}

pub const COMPAT_CELLS: [CompatCell; 18] = [
    CompatCell::new(
        CompatVendor::Cursor,
        CompatSurface::Skills,
        "ECHO_AGENT_CURSOR_SKILLS_ENABLED",
        Some(CompatRemoteKey::CursorSkills),
    ),
    CompatCell::new(
        CompatVendor::Cursor,
        CompatSurface::Rules,
        "ECHO_AGENT_CURSOR_RULES_ENABLED",
        Some(CompatRemoteKey::CursorRules),
    ),
    CompatCell::new(
        CompatVendor::Cursor,
        CompatSurface::Agents,
        "ECHO_AGENT_CURSOR_AGENTS_ENABLED",
        Some(CompatRemoteKey::CursorAgents),
    ),
    CompatCell::new(
        CompatVendor::Cursor,
        CompatSurface::Mcps,
        "ECHO_AGENT_CURSOR_MCPS_ENABLED",
        Some(CompatRemoteKey::CursorMcps),
    ),
    CompatCell::new(
        CompatVendor::Cursor,
        CompatSurface::Hooks,
        "ECHO_AGENT_CURSOR_HOOKS_ENABLED",
        Some(CompatRemoteKey::CursorHooks),
    ),
    CompatCell::new(
        CompatVendor::Cursor,
        CompatSurface::Sessions,
        "ECHO_AGENT_CURSOR_SESSIONS_ENABLED",
        Some(CompatRemoteKey::CursorSessions),
    ),
    CompatCell::new(
        CompatVendor::Claude,
        CompatSurface::Skills,
        "ECHO_AGENT_CLAUDE_SKILLS_ENABLED",
        Some(CompatRemoteKey::ClaudeSkills),
    ),
    CompatCell::new(
        CompatVendor::Claude,
        CompatSurface::Rules,
        "ECHO_AGENT_CLAUDE_RULES_ENABLED",
        Some(CompatRemoteKey::ClaudeRules),
    ),
    CompatCell::new(
        CompatVendor::Claude,
        CompatSurface::Agents,
        "ECHO_AGENT_CLAUDE_AGENTS_ENABLED",
        Some(CompatRemoteKey::ClaudeAgents),
    ),
    CompatCell::new(
        CompatVendor::Claude,
        CompatSurface::Mcps,
        "ECHO_AGENT_CLAUDE_MCPS_ENABLED",
        Some(CompatRemoteKey::ClaudeMcps),
    ),
    CompatCell::new(
        CompatVendor::Claude,
        CompatSurface::Hooks,
        "ECHO_AGENT_CLAUDE_HOOKS_ENABLED",
        Some(CompatRemoteKey::ClaudeHooks),
    ),
    CompatCell::new(
        CompatVendor::Claude,
        CompatSurface::Sessions,
        "ECHO_AGENT_CLAUDE_SESSIONS_ENABLED",
        Some(CompatRemoteKey::ClaudeSessions),
    ),
    CompatCell::new(
        CompatVendor::Codex,
        CompatSurface::Skills,
        "ECHO_AGENT_CODEX_SKILLS_ENABLED",
        None,
    ),
    CompatCell::new(
        CompatVendor::Codex,
        CompatSurface::Rules,
        "ECHO_AGENT_CODEX_RULES_ENABLED",
        None,
    ),
    CompatCell::new(
        CompatVendor::Codex,
        CompatSurface::Agents,
        "ECHO_AGENT_CODEX_AGENTS_ENABLED",
        None,
    ),
    CompatCell::new(
        CompatVendor::Codex,
        CompatSurface::Mcps,
        "ECHO_AGENT_CODEX_MCPS_ENABLED",
        None,
    ),
    CompatCell::new(
        CompatVendor::Codex,
        CompatSurface::Hooks,
        "ECHO_AGENT_CODEX_HOOKS_ENABLED",
        None,
    ),
    CompatCell::new(
        CompatVendor::Codex,
        CompatSurface::Sessions,
        "ECHO_AGENT_CODEX_SESSIONS_ENABLED",
        Some(CompatRemoteKey::CodexSessions),
    ),
];

/// Raw per-vendor compat cells parsed from `[compat.<vendor>]` TOML.
///
/// Resolution order is env override, this value, remote flag, default ON.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct VendorCompatToml {
    pub skills: Option<bool>,
    pub rules: Option<bool>,
    pub agents: Option<bool>,
    pub mcps: Option<bool>,
    pub hooks: Option<bool>,
    pub sessions: Option<bool>,
}

impl VendorCompatToml {
    fn value(&self, surface: CompatSurface) -> Option<bool> {
        match surface {
            CompatSurface::Skills => self.skills,
            CompatSurface::Rules => self.rules,
            CompatSurface::Agents => self.agents,
            CompatSurface::Mcps => self.mcps,
            CompatSurface::Hooks => self.hooks,
            CompatSurface::Sessions => self.sessions,
        }
    }
}

/// Raw `[compat]` TOML section.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct CompatConfigToml {
    #[serde(default)]
    pub cursor: VendorCompatToml,
    #[serde(default)]
    pub claude: VendorCompatToml,
    #[serde(default)]
    pub codex: VendorCompatToml,
}

impl CompatConfigToml {
    pub fn value(&self, cell: CompatCell) -> Option<bool> {
        match cell.vendor() {
            CompatVendor::Cursor => self.cursor.value(cell.surface()),
            CompatVendor::Claude => self.claude.value(cell.surface()),
            CompatVendor::Codex => self.codex.value(cell.surface()),
        }
    }
}

/// Resolved per-vendor compat cells. Plain bools — the runtime source of truth.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VendorCompat {
    pub skills: bool,
    pub rules: bool,
    pub agents: bool,
    pub mcps: bool,
    pub hooks: bool,
    pub sessions: bool,
}

impl VendorCompat {
    fn value(&self, surface: CompatSurface) -> bool {
        match surface {
            CompatSurface::Skills => self.skills,
            CompatSurface::Rules => self.rules,
            CompatSurface::Agents => self.agents,
            CompatSurface::Mcps => self.mcps,
            CompatSurface::Hooks => self.hooks,
            CompatSurface::Sessions => self.sessions,
        }
    }

    fn set(&mut self, surface: CompatSurface, value: bool) {
        match surface {
            CompatSurface::Skills => self.skills = value,
            CompatSurface::Rules => self.rules = value,
            CompatSurface::Agents => self.agents = value,
            CompatSurface::Mcps => self.mcps = value,
            CompatSurface::Hooks => self.hooks = value,
            CompatSurface::Sessions => self.sessions = value,
        }
    }
}

impl Default for VendorCompat {
    fn default() -> Self {
        Self {
            skills: true,
            rules: true,
            agents: true,
            mcps: true,
            hooks: true,
            sessions: true,
        }
    }
}

/// Resolved `[compat]` configuration threaded into compatibility consumers.
///
/// Every cell defaults on. Codex's non-session cells are reserved and are not
/// consumed by discovery.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CompatConfig {
    pub cursor: VendorCompat,
    pub claude: VendorCompat,
    pub codex: VendorCompat,
}

impl CompatConfig {
    pub fn value(&self, cell: CompatCell) -> bool {
        match cell.vendor() {
            CompatVendor::Cursor => self.cursor.value(cell.surface()),
            CompatVendor::Claude => self.claude.value(cell.surface()),
            CompatVendor::Codex => self.codex.value(cell.surface()),
        }
    }

    pub fn set(&mut self, cell: CompatCell, value: bool) {
        match cell.vendor() {
            CompatVendor::Cursor => self.cursor.set(cell.surface(), value),
            CompatVendor::Claude => self.claude.set(cell.surface(), value),
            CompatVendor::Codex => self.codex.set(cell.surface(), value),
        }
    }

    /// Config directories that may contain `skills/` subdirectories, in
    /// priority order. `.echo-agent` and `.agents` are always included; `.claude`
    /// and `.cursor` are gated on their respective `skills` cell.
    ///
    /// Replaces the hard-coded `[".echo-agent", ".agents", ".claude", ".cursor"]`
    /// in `collect_skill_config_dirs`. When all cells are on, the returned
    /// list is identical to the historical constant.
    pub fn skill_config_dirs(&self) -> Vec<&'static str> {
        let mut dirs = vec![".echo-agent", ".agents"];
        if self.claude.skills {
            dirs.push(".claude");
        }
        if self.cursor.skills {
            dirs.push(".cursor");
        }
        dirs
    }

    /// Subdirectories scanned for `*.md` rules files. `.echo-agent/rules` is always
    /// included; `.claude/rules` and `.cursor/rules` are gated on their
    /// respective `rules` cell.
    ///
    /// Replaces the hard-coded `RULES_DIRS` constant. When all cells are on,
    /// the returned list is identical.
    pub fn rules_dirs(&self) -> Vec<&'static str> {
        let mut dirs = vec![".echo-agent/rules"];
        if self.claude.rules {
            dirs.push(".claude/rules");
        }
        if self.cursor.rules {
            dirs.push(".cursor/rules");
        }
        dirs
    }

    /// Filenames (and relative paths) recognized as project-instruction files.
    /// The generic names are always included; the `.claude/`-prefixed entries
    /// are gated on `claude.agents`.
    ///
    /// Replaces the hard-coded `AGENT_FILENAMES` constant. When `claude.agents`
    /// is on, the returned list is identical (same order).
    pub fn agent_filenames(&self) -> Vec<&'static str> {
        let mut names = vec![
            "Agents.md",
            "Claude.md",
            "CLAUDE.md",
            "CLAUDE.local.md",
            "AGENT.md",
            "AGENTS.md",
        ];
        if self.claude.agents {
            names.push(".claude/CLAUDE.md");
            names.push(".claude/CLAUDE.local.md");
        }
        names
    }

    /// Home-level vendor directories scanned for AGENTS.md / rules files
    /// (e.g. `~/.claude`, `~/.cursor`). `.claude` is gated on `claude.agents`
    /// and `.cursor` on `cursor.agents`.
    ///
    /// Replaces the hard-coded `[".claude", ".cursor"]` home scan. When both
    /// cells are on, the returned list is identical (same order).
    pub fn agents_home_dirs(&self) -> Vec<&'static str> {
        let mut dirs = Vec::new();
        if self.claude.agents {
            dirs.push(".claude");
        }
        if self.cursor.agents {
            dirs.push(".cursor");
        }
        dirs
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_and_defaults_cover_every_cell() {
        use CompatRemoteKey::*;

        assert_eq!(
            COMPAT_CELLS.map(|cell| {
                (
                    cell.vendor().as_str(),
                    cell.surface().as_str(),
                    cell.remote_key(),
                )
            }),
            [
                ("cursor", "skills", Some(CursorSkills)),
                ("cursor", "rules", Some(CursorRules)),
                ("cursor", "agents", Some(CursorAgents)),
                ("cursor", "mcps", Some(CursorMcps)),
                ("cursor", "hooks", Some(CursorHooks)),
                ("cursor", "sessions", Some(CursorSessions)),
                ("claude", "skills", Some(ClaudeSkills)),
                ("claude", "rules", Some(ClaudeRules)),
                ("claude", "agents", Some(ClaudeAgents)),
                ("claude", "mcps", Some(ClaudeMcps)),
                ("claude", "hooks", Some(ClaudeHooks)),
                ("claude", "sessions", Some(ClaudeSessions)),
                ("codex", "skills", None),
                ("codex", "rules", None),
                ("codex", "agents", None),
                ("codex", "mcps", None),
                ("codex", "hooks", None),
                ("codex", "sessions", Some(CodexSessions)),
            ]
        );

        let defaults = CompatConfig::default();
        for cell in COMPAT_CELLS {
            assert!(
                defaults.value(cell),
                "{}.{}",
                cell.vendor().as_str(),
                cell.surface().as_str()
            );
        }
        for vendor in [defaults.cursor, defaults.claude, defaults.codex] {
            assert!(vendor.skills && vendor.rules && vendor.agents);
            assert!(vendor.mcps && vendor.hooks);
            assert!(vendor.sessions);
        }

        assert_eq!(
            COMPAT_CELLS
                .into_iter()
                .filter(|cell| cell.is_runtime_supported())
                .map(|cell| (cell.vendor().as_str(), cell.surface().as_str()))
                .collect::<Vec<_>>(),
            [
                ("cursor", "skills"),
                ("cursor", "rules"),
                ("cursor", "agents"),
                ("cursor", "mcps"),
                ("cursor", "hooks"),
                ("cursor", "sessions"),
                ("claude", "skills"),
                ("claude", "rules"),
                ("claude", "agents"),
                ("claude", "mcps"),
                ("claude", "hooks"),
                ("claude", "sessions"),
                ("codex", "sessions"),
            ]
        );
    }

    #[test]
    fn skill_config_dirs_all_on_matches_legacy_constant() {
        // Historical constant was `[".echo-agent", ".agents", ".claude", ".cursor"]`.
        assert_eq!(
            CompatConfig::default().skill_config_dirs(),
            vec![".echo-agent", ".agents", ".claude", ".cursor"]
        );
    }

    #[test]
    fn skill_config_dirs_gates_each_vendor() {
        let mut c = CompatConfig::default();
        c.cursor.skills = false;
        assert_eq!(
            c.skill_config_dirs(),
            vec![".echo-agent", ".agents", ".claude"]
        );

        c.claude.skills = false;
        assert_eq!(c.skill_config_dirs(), vec![".echo-agent", ".agents"]);

        // Only the `cursor` cell on (`claude` off): `cursor` still appended last.
        let mut c2 = CompatConfig::default();
        c2.claude.skills = false;
        assert_eq!(
            c2.skill_config_dirs(),
            vec![".echo-agent", ".agents", ".cursor"]
        );
    }

    #[test]
    fn rules_dirs_all_on_matches_legacy_constant() {
        // Historical `RULES_DIRS` was `[".echo-agent/rules", ".claude/rules", ".cursor/rules"]`.
        assert_eq!(
            CompatConfig::default().rules_dirs(),
            vec![".echo-agent/rules", ".claude/rules", ".cursor/rules"]
        );
    }

    #[test]
    fn rules_dirs_gates_each_vendor() {
        let mut c = CompatConfig::default();
        c.cursor.rules = false;
        assert_eq!(c.rules_dirs(), vec![".echo-agent/rules", ".claude/rules"]);
        c.claude.rules = false;
        assert_eq!(c.rules_dirs(), vec![".echo-agent/rules"]);
    }

    #[test]
    fn agent_filenames_all_on_matches_legacy_constant() {
        // Historical `AGENT_FILENAMES`.
        assert_eq!(
            CompatConfig::default().agent_filenames(),
            vec![
                "Agents.md",
                "Claude.md",
                "CLAUDE.md",
                "CLAUDE.local.md",
                "AGENT.md",
                "AGENTS.md",
                ".claude/CLAUDE.md",
                ".claude/CLAUDE.local.md",
            ]
        );
    }

    #[test]
    fn agent_filenames_drops_claude_subdir_when_off() {
        let mut c = CompatConfig::default();
        c.claude.agents = false;
        assert_eq!(
            c.agent_filenames(),
            vec![
                "Agents.md",
                "Claude.md",
                "CLAUDE.md",
                "CLAUDE.local.md",
                "AGENT.md",
                "AGENTS.md",
            ]
        );
    }

    #[test]
    fn agents_home_dirs_all_on_matches_legacy_constant() {
        // Historical home scan was `[".claude", ".cursor"]`.
        assert_eq!(
            CompatConfig::default().agents_home_dirs(),
            vec![".claude", ".cursor"]
        );
    }

    #[test]
    fn agents_home_dirs_gates_each_vendor() {
        let mut c = CompatConfig::default();
        c.claude.agents = false;
        assert_eq!(c.agents_home_dirs(), vec![".cursor"]);
        c.cursor.agents = false;
        assert!(c.agents_home_dirs().is_empty());
    }

    #[test]
    fn toml_struct_deserializes_partial_cells() {
        // The raw TOML struct is parsed from `[compat]` in the shell crate
        // (where `toml` is a dep). Here we exercise the same serde shape via
        // YAML (available in this crate) to pin the `Option<bool>` + `#[serde(default)]`
        // semantics: unset cells stay `None`, unset vendors default-construct.
        let parsed: CompatConfigToml = serde_yaml::from_str(
            "cursor:\n  skills: false\n  sessions: true\ncodex:\n  sessions: true\n",
        )
        .unwrap();
        assert_eq!(parsed.cursor.skills, Some(false));
        assert_eq!(parsed.cursor.rules, None);
        assert_eq!(parsed.cursor.sessions, Some(true));
        assert_eq!(parsed.claude, VendorCompatToml::default());
        assert_eq!(parsed.codex.sessions, Some(true));
        assert_eq!(parsed.codex.skills, None);

        // mcps cell round-trips the same way.
        let parsed: CompatConfigToml = serde_yaml::from_str("claude:\n  mcps: false\n").unwrap();
        assert_eq!(parsed.claude.mcps, Some(false));
        assert_eq!(parsed.claude.hooks, None);
        assert_eq!(parsed.claude.sessions, None);
        assert_eq!(parsed.cursor, VendorCompatToml::default());
        assert_eq!(parsed.codex, VendorCompatToml::default());
    }
}
