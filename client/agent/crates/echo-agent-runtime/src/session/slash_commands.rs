//! ACP slash command advertising and resolution.
use agent_client_protocol as acp;
use echo_agent_tools::implementations::echo_agent_build::LoopFireMode;
use echo_agent_tools::implementations::skills::skill::format_skill_name;
use echo_agent_tools::implementations::skills::types::SkillInfo;
use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;
/// A built-in slash command.
pub(crate) struct BuiltinCommand {
    pub name: &'static str,
    pub description: &'static str,
    pub argument_hint: Option<&'static str>,
    pub aliases: &'static [&'static str],
    /// Capability the agent must have for this command to be useful.
    /// Filtered by `CommandAvailability::allows()` at advertising time;
    /// commands that map to `BuiltinGate::AlwaysOn` are never gated.
    pub gate: BuiltinGate,
    workflow_projection: WorkflowProjection,
    resolve: fn(args: &str) -> BuiltinAction,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WorkflowProjection {
    None,
    ExactName,
}
/// Capability gate that decides whether a `BuiltinCommand` is advertised
/// and resolvable in a given session.
///
/// Each variant maps to a feature/tool the agent must actually have:
/// - `Memory`: a memory backend is configured (`SessionMemory::is_enabled`).
/// - `Scheduler`: `scheduler_create` is registered.
/// - `Hooks`: a hook registry is loaded.
/// - `Plugins`: a plugin registry is loaded.
/// - `Feedback`: the feedback manager is enabled.
/// - `MemoryConfigured`: memory backend params exist (may be currently
///   disabled). Used for `/memory` so the user can re-enable via toggle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BuiltinGate {
    AlwaysOn,
    Feedback,
    Memory,
    MemoryConfigured,
    /// Checks `scheduler_create` only. If any future shell-side builtin
    /// needs a separate scheduler-delete gate, add a `SchedulerDelete` variant.
    Scheduler,
    Hooks,
    Plugins,
    Goal,
    WorkflowLaunches,
    WorkflowManagement,
}
/// All built-in slash commands. Order here = display order in autocomplete.
pub(super) const BUILTIN_COMMANDS: &[BuiltinCommand] = &[
    BuiltinCommand {
        name: "compact",
        description: "Compress conversation history to save context window",
        argument_hint: Some("optional context about what to preserve"),
        aliases: &[],
        gate: BuiltinGate::AlwaysOn,
        workflow_projection: WorkflowProjection::None,
        resolve: |args| BuiltinAction::Compact {
            user_context: if args.is_empty() {
                None
            } else {
                Some(args.to_string())
            },
        },
    },
    BuiltinCommand {
        name: "always-approve",
        description: "Toggle always-approve mode (skip all permission prompts)",
        argument_hint: Some("on|off"),
        aliases: &["yolo"],
        gate: BuiltinGate::AlwaysOn,
        workflow_projection: WorkflowProjection::None,
        resolve: |args| BuiltinAction::SetYolo {
            enabled: !matches!(
                args.to_lowercase().as_str(),
                "off" | "false" | "0" | "no" | "disable"
            ),
        },
    },
    BuiltinCommand {
        name: "flush",
        description: "Flush conversation memory to disk now",
        argument_hint: None,
        aliases: &[],
        gate: BuiltinGate::Memory,
        workflow_projection: WorkflowProjection::None,
        resolve: |_args| BuiltinAction::FlushMemory,
    },
    BuiltinCommand {
        name: "dream",
        description: "Run memory consolidation (merge session logs into organized topics)",
        argument_hint: None,
        aliases: &[],
        gate: BuiltinGate::Memory,
        workflow_projection: WorkflowProjection::None,
        resolve: |_args| BuiltinAction::Dream,
    },
    BuiltinCommand {
        name: "memory",
        description: "Browse, view, and manage your memories",
        argument_hint: Some("on|off"),
        aliases: &["mem"],
        gate: BuiltinGate::MemoryConfigured,
        workflow_projection: WorkflowProjection::None,
        resolve: |args| {
            let trimmed = args.trim().to_lowercase();
            match trimmed.as_str() {
                "on" | "enable" => BuiltinAction::MemoryToggle { enabled: true },
                "off" | "disable" => BuiltinAction::MemoryToggle { enabled: false },
                _ => BuiltinAction::MemoryBrowse,
            }
        },
    },
    BuiltinCommand {
        name: "context",
        description: "Show context window usage and session stats",
        argument_hint: None,
        aliases: &[],
        gate: BuiltinGate::AlwaysOn,
        workflow_projection: WorkflowProjection::None,
        resolve: |_args| BuiltinAction::ContextInfo,
    },
    BuiltinCommand {
        name: "hooks-trust",
        description: "Trust this project for hook execution",
        argument_hint: None,
        aliases: &[],
        gate: BuiltinGate::Hooks,
        workflow_projection: WorkflowProjection::None,
        resolve: |_args| BuiltinAction::HooksTrust,
    },
    BuiltinCommand {
        name: "hooks-list",
        description: "Show hooks loaded in this session",
        argument_hint: None,
        aliases: &[],
        gate: BuiltinGate::Hooks,
        workflow_projection: WorkflowProjection::None,
        resolve: |_args| BuiltinAction::HooksList,
    },
    BuiltinCommand {
        name: "hooks-add",
        description: "Add a custom hook file or directory",
        argument_hint: Some("path to hook file or directory"),
        aliases: &[],
        gate: BuiltinGate::Hooks,
        workflow_projection: WorkflowProjection::None,
        resolve: |args| BuiltinAction::HooksAdd {
            path: args.trim().to_string(),
        },
    },
    BuiltinCommand {
        name: "hooks-remove",
        description: "Remove a custom hook file or directory path",
        argument_hint: Some("path to hook file or directory"),
        aliases: &[],
        gate: BuiltinGate::Hooks,
        workflow_projection: WorkflowProjection::None,
        resolve: |args| BuiltinAction::HooksRemove {
            path: args.trim().to_string(),
        },
    },
    BuiltinCommand {
        name: "hooks-untrust",
        description: "Remove trust for the current project",
        argument_hint: None,
        aliases: &[],
        gate: BuiltinGate::Hooks,
        workflow_projection: WorkflowProjection::None,
        resolve: |_args| BuiltinAction::HooksUntrust,
    },
    BuiltinCommand {
        name: "plugins",
        description: "Manage plugins (list, reload, trust, add, remove)",
        argument_hint: Some("list | reload | trust <path> | add <path> | remove <path>"),
        aliases: &["plugin"],
        gate: BuiltinGate::Plugins,
        workflow_projection: WorkflowProjection::None,
        resolve: |args| {
            let trimmed = args.trim();
            if trimmed.is_empty() || trimmed == "list" {
                BuiltinAction::PluginsList
            } else if trimmed == "reload" {
                BuiltinAction::PluginsReload
            } else if trimmed.starts_with("trust") {
                BuiltinAction::PluginsTrust
            } else if let Some(path) = trimmed.strip_prefix("add ") {
                BuiltinAction::PluginsAdd {
                    path: path.trim().to_string(),
                }
            } else if let Some(path) = trimmed.strip_prefix("remove ") {
                BuiltinAction::PluginsRemove {
                    path: path.trim().to_string(),
                }
            } else if let Some(args) = trimmed.strip_prefix("install ") {
                let args = args.trim();
                let trust = args.ends_with(" --trust") || args == "--trust";
                let source = if trust {
                    args.trim_end_matches(" --trust").trim().to_string()
                } else {
                    args.to_string()
                };
                BuiltinAction::PluginsInstall { source, trust }
            } else if let Some(args) = trimmed.strip_prefix("uninstall ") {
                let args = args.trim();
                let confirm = args.ends_with(" --confirm") || args == "--confirm";
                let name = if confirm {
                    args.trim_end_matches(" --confirm").trim().to_string()
                } else {
                    args.to_string()
                };
                BuiltinAction::PluginsUninstall { name, confirm }
            } else if trimmed == "update" {
                BuiltinAction::PluginsUpdate { name: None }
            } else if let Some(name) = trimmed.strip_prefix("update ") {
                BuiltinAction::PluginsUpdate {
                    name: Some(name.trim().to_string()),
                }
            } else {
                BuiltinAction::PluginsList
            }
        },
    },
    BuiltinCommand {
        name: "reload-plugins",
        description: "Reload plugins from disk (alias for /plugins reload)",
        argument_hint: None,
        aliases: &[],
        gate: BuiltinGate::Plugins,
        workflow_projection: WorkflowProjection::None,
        resolve: |_args| BuiltinAction::PluginsReload,
    },
    BuiltinCommand {
        name: "session-info",
        description: "Show session details (model, turns, context usage)",
        argument_hint: None,
        aliases: &["status", "info"],
        gate: BuiltinGate::AlwaysOn,
        workflow_projection: WorkflowProjection::None,
        resolve: |_args| BuiltinAction::SessionInfo,
    },
    BuiltinCommand {
        name: "feedback",
        description: "Send feedback about the current session",
        argument_hint: Some("feedback text"),
        aliases: &[],
        gate: BuiltinGate::Feedback,
        workflow_projection: WorkflowProjection::None,
        resolve: |args| BuiltinAction::Feedback {
            text: args.trim().to_string(),
        },
    },
    BuiltinCommand {
        name: "deep-research",
        description: "Research with bounded parallel agents, cross-check evidence, and write a cited report",
        argument_hint: Some("<query>"),
        aliases: &[],
        gate: BuiltinGate::WorkflowLaunches,
        workflow_projection: WorkflowProjection::ExactName,
        resolve: |args| BuiltinAction::DeepResearch {
            query: args.trim().to_string(),
        },
    },
    BuiltinCommand {
        name: "workflow",
        description: "Launch a saved workflow, list runs, or manage a run (pause, resume, stop, save)",
        argument_hint: Some(
            "<name> [--agent-budget N] [--effort LEVEL] [args] | runs | pause|resume|stop|save [name]",
        ),
        aliases: &[],
        gate: BuiltinGate::WorkflowManagement,
        workflow_projection: WorkflowProjection::None,
        resolve: |args| {
            const OPS: [&str; 4] = ["pause", "resume", "stop", "save"];
            let trimmed = args.trim();
            let mut parts = trimmed.split_whitespace();
            let first = parts.next().unwrap_or_default();
            let second = parts.next().unwrap_or_default();
            let first_is_op = OPS.contains(&first.to_lowercase().as_str());
            let first_is_runs = first.eq_ignore_ascii_case("runs") && second.is_empty();
            let second_is_final_op =
                OPS.contains(&second.to_lowercase().as_str()) && parts.next().is_none();
            if first.is_empty() || first_is_op || first_is_runs || second_is_final_op {
                let (op, run_id) = if first_is_op {
                    (
                        first.to_lowercase(),
                        trimmed[first.len()..].trim_start().to_string(),
                    )
                } else if first_is_runs {
                    ("runs".to_string(), String::new())
                } else if second_is_final_op {
                    (second.to_lowercase(), first.to_string())
                } else {
                    (String::new(), String::new())
                };
                BuiltinAction::WorkflowManage { run_id, op }
            } else {
                BuiltinAction::WorkflowLaunch {
                    name: first.to_string(),
                    input: trimmed[first.len()..].trim_start().to_string(),
                }
            }
        },
    },
    BuiltinCommand {
        name: "goal",
        description: "Set, manage, or check an autonomous goal",
        argument_hint: Some("<objective> [--budget <tokens>] | status | pause | resume | clear"),
        aliases: &[],
        gate: BuiltinGate::Goal,
        workflow_projection: WorkflowProjection::None,
        resolve: |args| {
            let trimmed = args.trim();
            match trimmed.to_lowercase().as_str() {
                "" | "status" => BuiltinAction::GoalStatus,
                "pause" => BuiltinAction::GoalPause,
                "resume" => BuiltinAction::GoalResume,
                "clear" => BuiltinAction::GoalClear,
                _ => {
                    let (objective, token_budget) = parse_goal_budget(trimmed);
                    BuiltinAction::GoalSet {
                        objective,
                        token_budget,
                    }
                }
            }
        },
    },
];
/// Split a trailing `--budget <tokens>` flag off a `/goal` objective.
///
/// Only a TRAILING, standalone flag is consumed: the flag must be its own
/// whitespace-separated token and the value a final all-digit positive
/// token. Anything else stays part of the objective so a goal text that
/// merely mentions the flag is never silently mangled.
fn parse_goal_budget(trimmed: &str) -> (String, Option<i64>) {
    if let Some((head, tail)) = trimmed.rsplit_once("--budget") {
        let value = tail.trim();
        let flag_is_own_token = head.ends_with(char::is_whitespace)
            && tail.starts_with(char::is_whitespace)
            && !value.contains(char::is_whitespace);
        let head = head.trim_end();
        if flag_is_own_token
            && !head.is_empty()
            && !value.is_empty()
            && value.bytes().all(|b| b.is_ascii_digit())
            && let Ok(budget) = value.parse::<i64>()
            && budget > 0
        {
            return (head.to_string(), Some(budget));
        }
    }
    (trimmed.to_string(), None)
}
const PROMPT_COMMANDS: &[BuiltinCommand] = &[BuiltinCommand {
    name: "loop",
    description: "Run a prompt on a recurring interval",
    argument_hint: Some("[interval] <prompt>"),
    aliases: &[],
    gate: BuiltinGate::Scheduler,
    workflow_projection: WorkflowProjection::None,
    resolve: |_| unreachable!("/loop is dispatched via the PROMPT_COMMANDS path in resolve()"),
}];
/// Per-session capability snapshot used to gate which built-in slash
/// commands the shell advertises and resolves.
///
/// Each field corresponds to a `BuiltinGate` variant. Construct via
/// `CommandAvailability::all_enabled()` for tests, or build it from a
/// live `SessionActor` (see the call site in `acp_session.rs`).
///
/// `Default` returns every gate disabled (fail-closed) so a forgotten
/// initialization advertises only `BuiltinGate::AlwaysOn` commands.
/// In test code, prefer `all_enabled()` when the gating itself isn't
/// under test -- otherwise the test will silently lose coverage of any
/// gated builtin.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct CommandAvailability {
    pub feedback: bool,
    /// Memory backend is enabled AND the active toolset includes the
    /// memory read tools. `/flush` and `/dream` only make sense when the
    /// model can later read back what they wrote, so the read-side tool
    /// presence is the right signal -- harnesses that don't register
    /// `memory_search`/`memory_get` get the commands hidden without the
    /// gating layer needing to know about agent_type.
    pub memory: bool,
    /// Memory backend is configured (has `backend_params`) but not
    /// necessarily currently enabled. Gates `/memory` (browse + toggle)
    /// so the user can re-enable memory after toggling it off.
    pub memory_configured: bool,
    pub scheduler: bool,
    pub hooks: bool,
    pub plugins: bool,
    pub goal: bool,
    pub workflows: bool,
    pub workflow_management: bool,
}
impl CommandAvailability {
    /// `true` if commands gated on `gate` should be advertised this session.
    pub(crate) fn allows(&self, gate: BuiltinGate) -> bool {
        match gate {
            BuiltinGate::AlwaysOn => true,
            BuiltinGate::Feedback => self.feedback,
            BuiltinGate::Memory => self.memory,
            BuiltinGate::MemoryConfigured => self.memory_configured,
            BuiltinGate::Scheduler => self.scheduler,
            BuiltinGate::Hooks => self.hooks,
            BuiltinGate::Plugins => self.plugins,
            BuiltinGate::Goal => self.goal,
            BuiltinGate::WorkflowLaunches => self.workflows,
            BuiltinGate::WorkflowManagement => self.workflows || self.workflow_management,
        }
    }
    /// Test helper: every gate satisfied (matches the legacy "feedback only"
    /// fixture but enables every newly-gated command too).
    #[cfg(test)]
    pub(crate) fn all_enabled() -> Self {
        Self {
            feedback: true,
            memory: true,
            memory_configured: true,
            scheduler: true,
            hooks: true,
            plugins: true,
            goal: true,
            workflows: true,
            workflow_management: true,
        }
    }
}
/// Build the JSON value for `AvailableCommandsUpdate.meta` containing the
/// agent's currently-registered tool names.
///
/// Wire format: `{"tools": ["read_file", "scheduler_create", ...]}`.
/// Pager clients drain this and call `CommandRegistry::set_available_tools`
/// to gate tool-dependent commands like `/loop`.
///
/// Takes `&[String]` rather than `&[&str]` because serde_json copies
/// each entry into the `Value` regardless, so an intermediate
/// `Vec<&str>` adapter would just waste an allocation.
pub(crate) fn build_tools_meta(tool_names: &[String]) -> acp::Meta {
    let mut meta = acp::Meta::new();
    meta.insert("tools".to_owned(), serde_json::json!(tool_names));
    meta
}
/// Pager-owned slash trigger keys (canonical + aliases) plus shell command
/// names the pager never offers (`hooks-add`, `reload-plugins`, …). Burned
/// when advertising skills so a colliding skill ships qualified (`acme:login`,
/// `local:hooks-add`) instead of a bare name the pager will drop.
///
/// Synced by pager contract tests (`pager_builtin_triggers_are_reserved_in_shell`,
/// `pager_blocked_acp_names_are_reserved_in_shell`). Add names here when adding
/// a pager builtin or a pager-blocked shell command.
pub const PAGER_COMMAND_KEYS: &[&str] = &[
    "agents",
    "agents-dashboard",
    "always-approve",
    "announcements",
    "auto",
    "btw",
    "cd",
    "changelog",
    "chat",
    "clear",
    "cloud",
    "compact",
    "compact-mode",
    "config",
    "config-agents",
    "context",
    "copy",
    "cost",
    "dashboard",
    "debug",
    "delete",
    "docs",
    "doctor",
    "edit-prompt",
    "effort",
    "exit",
    "expand",
    "export",
    "feedback",
    "find",
    "fork",
    "full",
    "fullscreen",
    "gboom",
    "guides",
    "help",
    "history",
    "home",
    "hooks",
    "hooks-add",
    "hooks-list",
    "hooks-remove",
    "hooks-trust",
    "hooks-untrust",
    "howto",
    "imagine",
    "imagine-video",
    "import-claude",
    "jump",
    "login",
    "logout",
    "log",
    "loop",
    "m",
    "marketplace",
    "mcps",
    "minimal",
    "ml",
    "model",
    "multiline",
    "new",
    "onboarding",
    "personas",
    "plan",
    "plan-view",
    "plugin",
    "plugins",
    "preferences",
    "prefs",
    "privacy",
    "queue",
    "quit",
    "recap",
    "release-notes",
    "reload-plugins",
    "remember",
    "rename",
    "resume",
    "rewind",
    "scroll-debug",
    "session-info",
    "sessions",
    "settings",
    "share",
    "show-plan",
    "skills",
    "summarize",
    "tasks",
    "terminal-check",
    "terminal-info",
    "terminal-setup",
    "theme",
    "timeline",
    "timestamps",
    "title",
    "toggle-mouse-reporting",
    "tour",
    "transcript",
    "tutorial",
    "t",
    "undo",
    "usage",
    "view-plan",
    "vim-mode",
    "voice",
    "welcome",
    "workflow",
    "workflows",
    "yolo",
];
/// Unconditional reservations for `echoagent inspect`. Live advertising still
/// includes currently gated-on shell builtins plus [`PAGER_COMMAND_KEYS`].
static RESERVED_SLASH_NAMES: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    let mut taken: HashSet<&'static str> = PAGER_COMMAND_KEYS.iter().copied().collect();
    for builtin in BUILTIN_COMMANDS
        .iter()
        .chain(PROMPT_COMMANDS.iter())
        .filter(|builtin| builtin.gate == BuiltinGate::AlwaysOn)
    {
        taken.insert(builtin.name);
        taken.extend(builtin.aliases.iter().copied());
    }
    taken
});
/// Pager `CommandRegistry::apply_acp_commands` lowercases ACP names before
/// reservation / dedup. Catalog keys, resolve, and inspect must fold the
/// same way or a SKILL.md name like `Login` is advertised bare and dropped.
fn slash_key(name: &str) -> String {
    name.to_lowercase()
}
pub(crate) fn is_reserved_slash_name(name: &str) -> bool {
    RESERVED_SLASH_NAMES.contains(slash_key(name).as_str())
}
struct EffectiveCommandCatalog<'a> {
    builtins: Vec<&'a BuiltinCommand>,
    skills: Vec<SkillCommand<'a>>,
    workflows: Vec<&'a crate::session::workflow::registry::WorkflowListing>,
}
struct SkillCommand<'a> {
    name: String,
    skill: &'a SkillInfo,
}
fn exact_workflow_projection<'a>(
    command: &BuiltinCommand,
    workflows: &'a [crate::session::workflow::registry::WorkflowListing],
) -> Option<&'a crate::session::workflow::registry::WorkflowListing> {
    match command.workflow_projection {
        WorkflowProjection::None => None,
        WorkflowProjection::ExactName => {
            let mut matches = workflows
                .iter()
                .filter(|workflow| workflow.name == command.name);
            let workflow = matches.next()?;
            matches.next().is_none().then_some(workflow)
        }
    }
}
fn workflow_meta(workflow: &crate::session::workflow::registry::WorkflowListing) -> acp::Meta {
    let mut meta = acp::Meta::new();
    meta.insert(
        "workflowSource".to_string(),
        serde_json::json!(workflow.source),
    );
    if let Some(path) = &workflow.path {
        meta.insert("workflowPath".to_string(), serde_json::json!(path));
    }
    meta
}
impl<'a> EffectiveCommandCatalog<'a> {
    fn build(
        skills: &'a [SkillInfo],
        availability: CommandAvailability,
        workflows: &'a [crate::session::workflow::registry::WorkflowListing],
    ) -> Self {
        let builtins: Vec<_> = BUILTIN_COMMANDS
            .iter()
            .chain(PROMPT_COMMANDS.iter())
            .filter(|builtin| availability.allows(builtin.gate))
            .collect();
        let mut taken: HashSet<String> = builtins
            .iter()
            .flat_map(|builtin| {
                std::iter::once(builtin.name).chain(builtin.aliases.iter().copied())
            })
            .chain(PAGER_COMMAND_KEYS.iter().copied())
            .map(slash_key)
            .collect();
        let candidates: Vec<_> = skills
            .iter()
            .filter(|skill| skill.user_invocable && skill.enabled)
            .collect();
        let mut bare_counts: HashMap<String, usize> = HashMap::new();
        let mut qualified_counts: HashMap<String, usize> = HashMap::new();
        for skill in &candidates {
            *bare_counts.entry(slash_key(&skill.name)).or_default() += 1;
            *qualified_counts
                .entry(slash_key(&format_skill_name(skill)))
                .or_default() += 1;
        }
        let mut effective_skills = Vec::new();
        for skill in candidates {
            let bare_key = slash_key(&skill.name);
            let name = if bare_counts.get(&bare_key) == Some(&1) && !taken.contains(&bare_key) {
                bare_key
            } else {
                let qualified = slash_key(&format_skill_name(skill));
                if qualified_counts.get(&qualified) != Some(&1) || taken.contains(&qualified) {
                    tracing::debug!(
                        skill = %skill.name,
                        path = %skill.path,
                        %qualified,
                        "skill not advertised: both its bare and qualified names are taken"
                    );
                    continue;
                }
                qualified
            };
            taken.insert(name.clone());
            effective_skills.push(SkillCommand { name, skill });
        }
        taken.extend(bare_counts.keys().cloned());
        let effective_workflows = if availability.allows(BuiltinGate::WorkflowLaunches) {
            let mut counts: HashMap<String, usize> = HashMap::new();
            for workflow in workflows {
                *counts.entry(slash_key(&workflow.name)).or_default() += 1;
            }
            workflows
                .iter()
                .filter(|workflow| {
                    let key = slash_key(&workflow.name);
                    let advertisable = counts.get(&key) == Some(&1) && !taken.contains(&key);
                    if !advertisable {
                        tracing::debug!(
                            workflow = %workflow.name,
                            "workflow not advertised: name is taken by a command or skill"
                        );
                    }
                    advertisable
                })
                .collect()
        } else {
            Vec::new()
        };
        Self {
            builtins,
            skills: effective_skills,
            workflows: effective_workflows,
        }
    }
    /// Skill by its advertised (effective) name.
    fn skill(&self, name: &str) -> Option<&'a SkillInfo> {
        let key = slash_key(name);
        self.skills
            .iter()
            .find(|command| command.name == key)
            .map(|command| command.skill)
    }
    /// Advertised name, else the canonical qualified form. Leading-token
    /// only — mid-prose `/word` matches advertised names so an unadvertised
    /// spelling can't hijack sentence tails.
    fn skill_resolvable(&self, name: &str) -> Option<&'a SkillInfo> {
        self.skill(name).or_else(|| {
            let key = slash_key(name);
            self.skills
                .iter()
                .find(|command| slash_key(&format_skill_name(command.skill)) == key)
                .map(|command| command.skill)
        })
    }
    fn workflow(
        &self,
        name: &str,
    ) -> Option<&'a crate::session::workflow::registry::WorkflowListing> {
        let key = slash_key(name);
        self.workflows
            .iter()
            .copied()
            .find(|workflow| slash_key(&workflow.name) == key)
    }
}
/// Build the ACP `AvailableCommand` list for the client autocomplete menu.
///
/// Skills include `scope` and `path` in `_meta` so the client can show
/// where the command comes from (e.g. "project" vs "global") and link
/// to the SKILL.md source.
pub(super) fn available_commands(
    skills: &[SkillInfo],
    availability: CommandAvailability,
    workflows: &[crate::session::workflow::registry::WorkflowListing],
) -> Vec<acp::AvailableCommand> {
    let catalog = EffectiveCommandCatalog::build(skills, availability, workflows);
    let mut commands =
        Vec::with_capacity(catalog.builtins.len() + catalog.skills.len() + catalog.workflows.len());
    commands.extend(catalog.builtins.iter().map(|builtin| {
        acp::AvailableCommand::new(builtin.name.to_string(), builtin.description.to_string())
            .input(builtin.argument_hint.map(|hint| {
                acp::AvailableCommandInput::Unstructured(acp::UnstructuredCommandInput::new(
                    hint.to_string(),
                ))
            }))
            .meta(exact_workflow_projection(builtin, workflows).map(workflow_meta))
    }));
    commands.extend(catalog.skills.iter().map(|command| {
        let skill = command.skill;
        let mut meta_map = serde_json::Map::new();
        meta_map.insert("scope".into(), serde_json::json!(skill.scope));
        meta_map.insert("path".into(), serde_json::json!(skill.path));
        meta_map.insert("bareName".into(), serde_json::json!(skill.name));
        meta_map.insert(
            "qualifiedName".into(),
            serde_json::json!(slash_key(&format_skill_name(skill))),
        );
        if let Some(ref plugin_name) = skill.plugin_name {
            meta_map.insert("pluginName".into(), serde_json::json!(plugin_name));
        }
        if let Some(display_name) = skill.display_name.as_deref().filter(|s| !s.is_empty()) {
            meta_map.insert("displayName".into(), serde_json::json!(display_name));
        }
        if let Some(extra) = &skill.metadata {
            for key in ["product", "icon"] {
                if let Some(value) = extra.get(key) {
                    meta_map.insert(key.to_string(), serde_json::json!(value));
                }
            }
        }
        acp::AvailableCommand::new(
            command.name.clone(),
            skill
                .short_description
                .as_deref()
                .unwrap_or(&skill.description)
                .to_string(),
        )
        .input(skill.argument_hint.as_ref().map(|hint| {
            acp::AvailableCommandInput::Unstructured(acp::UnstructuredCommandInput::new(
                hint.clone(),
            ))
        }))
        .meta(Some(meta_map))
    }));
    commands.extend(catalog.workflows.iter().map(|workflow| {
        let meta = serde_json::json!({
            "workflowSource": workflow.source,
            "workflowPath": workflow.path,
        })
        .as_object()
        .cloned();
        acp::AvailableCommand::new(
            workflow.name.clone(),
            format!("Workflow: {}", workflow.description),
        )
        .input(Some(acp::AvailableCommandInput::Unstructured(
            acp::UnstructuredCommandInput::new(
                "[--agent-budget N] [--effort LEVEL] [args]".to_string(),
            ),
        )))
        .meta(meta)
    }));
    commands
}
/// Pre-session builtin commands for `InitializeResponse._meta`.
///
/// Advertises every always-on command plus any gated command whose gate
/// is satisfied by `availability`. Pre-session, only config-derived gates
/// (e.g. `goal`, which is driven by the `resolve_goal()` feature flag and
/// not by a live toolset) can be evaluated; runtime/tool-dependent gates
/// stay closed because there's no session context yet. See
/// `MvpAgent::command_availability` for how the pre-session snapshot is
/// built. With `CommandAvailability::default()` (all gates closed) this
/// is equivalent to advertising only `BuiltinGate::AlwaysOn` commands.
pub(crate) fn builtin_commands(availability: CommandAvailability) -> Vec<acp::AvailableCommand> {
    BUILTIN_COMMANDS
        .iter()
        .filter(|cmd| availability.allows(cmd.gate))
        .map(|cmd| {
            acp::AvailableCommand::new(cmd.name.to_string(), cmd.description.to_string()).input(
                cmd.argument_hint.map(|hint| {
                    acp::AvailableCommandInput::Unstructured(acp::UnstructuredCommandInput::new(
                        hint.to_string(),
                    ))
                }),
            )
        })
        .collect()
}
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ListCommandsRequest {
    #[serde(default)]
    pub session_id: Option<acp::SessionId>,
    #[serde(default)]
    pub cwd: Option<String>,
    /// Product lane: `"chat"` filters to EchoAgent Chat / EchoAgent Computer first-party
    /// skills only. Omitted or any other value keeps the full Build catalog.
    #[serde(default)]
    pub kind: Option<String>,
}
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct ListCommandsResponse {
    pub commands: Vec<acp::AvailableCommand>,
    /// Live-session tool names (`None` = unknown / pre-session). Same set as
    /// `AvailableCommandsUpdate.meta.tools`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<String>>,
}
/// Last successful product catalog shared by ACU, `commands/list(kind=chat)`,
/// and chat slash resolve. Matched by auth token / user id / team / org so
/// OIDC enrichment and team switches never leak another context's menu.
static PRODUCT_SKILLS_CACHE: parking_lot::Mutex<Option<ProductSkillsCacheEntry>> =
    parking_lot::Mutex::new(None);
/// Short-lived degraded (user-list failed) catalog. Separate from success cache
/// so incomplete menus are not pinned for the full success TTL.
static PRODUCT_SKILLS_DEGRADED_CACHE: parking_lot::Mutex<Option<ProductSkillsCacheEntry>> =
    parking_lot::Mutex::new(None);
/// Short-lived total-failure marker so cold ACU/resolve during an outage does
/// not re-run the full REST ladder every turn.
static PRODUCT_SKILLS_NEGATIVE_CACHE: parking_lot::Mutex<Option<ProductSkillsIdentityStamp>> =
    parking_lot::Mutex::new(None);
/// Coalesce concurrent catalog fetches (ACU + list + resolve) into one REST
/// ladder. Callers re-check caches after acquiring the gate.
static PRODUCT_SKILLS_FETCH_GATE: std::sync::OnceLock<tokio::sync::Mutex<()>> =
    std::sync::OnceLock::new();
/// Fresh successful catalog is reused without another REST round-trip so ACU,
/// list, and per-turn resolve do not stampede cloud.echo-agent.invalid.
const PRODUCT_SKILLS_SUCCESS_TTL: std::time::Duration = std::time::Duration::from_secs(60);
/// Bounded negative cache for user-list failure (bundled-only). Keeps consumers
/// from re-paying the full retry ladder during a short outage.
const PRODUCT_SKILLS_DEGRADED_TTL: std::time::Duration = std::time::Duration::from_secs(10);
/// Bounded negative cache for total catalog Err (bundled failed after retries).
const PRODUCT_SKILLS_NEGATIVE_TTL: std::time::Duration = std::time::Duration::from_secs(10);
/// Locale for product Skills REST. Catalog is English-only today.
const PRODUCT_SKILLS_LOCALE: &str = "en";
#[derive(Clone)]
struct ProductSkillsCacheEntry {
    auth_key: String,
    user_id: String,
    team_id: Option<String>,
    organization_id: Option<String>,
    skills: Vec<SkillInfo>,
    fetched_at: std::time::Instant,
}
/// Identity stamp without skills payload (negative / Err cache).
#[derive(Clone)]
struct ProductSkillsIdentityStamp {
    auth_key: String,
    user_id: String,
    team_id: Option<String>,
    organization_id: Option<String>,
    fetched_at: std::time::Instant,
}
fn product_skills_identity_matches(
    auth_key: &str,
    user_id: &str,
    team_id: &Option<String>,
    organization_id: &Option<String>,
    auth: &crate::auth::EchoAgentAuth,
) -> bool {
    if team_id != &auth.team_id || organization_id != &auth.organization_id {
        return false;
    }
    if !auth.key.is_empty() && auth_key == auth.key {
        return true;
    }
    if !auth.user_id.is_empty() && !user_id.is_empty() && user_id == auth.user_id {
        return true;
    }
    false
}
fn product_skills_cache_matches(
    entry: &ProductSkillsCacheEntry,
    auth: &crate::auth::EchoAgentAuth,
) -> bool {
    product_skills_identity_matches(
        &entry.auth_key,
        &entry.user_id,
        &entry.team_id,
        &entry.organization_id,
        auth,
    )
}
fn product_skills_negative_matches(
    entry: &ProductSkillsIdentityStamp,
    auth: &crate::auth::EchoAgentAuth,
) -> bool {
    product_skills_identity_matches(
        &entry.auth_key,
        &entry.user_id,
        &entry.team_id,
        &entry.organization_id,
        auth,
    )
}
fn product_skills_cache_entry(
    auth: &crate::auth::EchoAgentAuth,
    skills: Vec<SkillInfo>,
) -> ProductSkillsCacheEntry {
    ProductSkillsCacheEntry {
        auth_key: auth.key.clone(),
        user_id: auth.user_id.clone(),
        team_id: auth.team_id.clone(),
        organization_id: auth.organization_id.clone(),
        skills,
        fetched_at: std::time::Instant::now(),
    }
}
/// Success-cache write after a catalog fetch.
///
/// Always keys by the **primary** auth identity (user + team/org), even when
/// the HTTP request succeeded via an untagged recovery credential. That lets
/// the same team primary hit TTL without re-running the 403 ladder, while
/// personal (empty team/org) primaries cannot match a team-keyed entry.
fn product_skills_cache_entry_after_fetch(
    primary: &crate::auth::EchoAgentAuth,
    skills: Vec<SkillInfo>,
    _used_untagged_recovery: bool,
) -> ProductSkillsCacheEntry {
    product_skills_cache_entry(primary, skills)
}
fn product_skills_negative_stamp(auth: &crate::auth::EchoAgentAuth) -> ProductSkillsIdentityStamp {
    ProductSkillsIdentityStamp {
        auth_key: auth.key.clone(),
        user_id: auth.user_id.clone(),
        team_id: auth.team_id.clone(),
        organization_id: auth.organization_id.clone(),
        fetched_at: std::time::Instant::now(),
    }
}
fn clear_degraded_cache_for_auth(auth: &crate::auth::EchoAgentAuth) {
    let mut guard = PRODUCT_SKILLS_DEGRADED_CACHE.lock();
    if let Some(entry) = guard.as_ref()
        && product_skills_cache_matches(entry, auth)
    {
        *guard = None;
    }
}
fn clear_negative_cache_for_auth(auth: &crate::auth::EchoAgentAuth) {
    let mut guard = PRODUCT_SKILLS_NEGATIVE_CACHE.lock();
    if let Some(entry) = guard.as_ref()
        && product_skills_negative_matches(entry, auth)
    {
        *guard = None;
    }
}
fn product_skills_fetch_gate() -> &'static tokio::sync::Mutex<()> {
    PRODUCT_SKILLS_FETCH_GATE.get_or_init(|| tokio::sync::Mutex::new(()))
}
#[cfg(test)]
pub(crate) fn clear_product_skills_cache_for_test() {
    *PRODUCT_SKILLS_CACHE.lock() = None;
    *PRODUCT_SKILLS_DEGRADED_CACHE.lock() = None;
    *PRODUCT_SKILLS_NEGATIVE_CACHE.lock() = None;
}
/// Product (cloud.echo-agent.invalid) Skills catalog as SkillInfo rows for slash advertising
/// and chat-kind slash resolve / skill expansion.
///
/// Shared by `list_commands(kind=chat)`, chat-session
/// `available_commands_update`, and turn/interjection skill resolution.
/// Never substitutes Build disk skills.
///
/// Catalog source is product Skills REST (see `remote::skills_client`), not
/// gateway `conversation.commands.updated` — one process-local source for ACU
/// and shell-side resolve without a gateway bridge.
///
/// - `Some(skills)` — REST succeeded (possibly empty; empty 200 is authoritative),
///   a fresh in-TTL success cache hit, a short-TTL degraded (user-list failed)
///   hit, or a prior successful catalog for **this** auth identity reused after
///   a transient failure
/// - `None` — no auth, REST failed with no matching cached catalog, or logout
pub(crate) async fn product_skill_infos(
    auth: Option<std::sync::Arc<crate::auth::AuthManager>>,
) -> Option<Vec<SkillInfo>> {
    let Some(auth) = auth else {
        tracing::warn!("product skills: no auth — catalog unavailable");
        return None;
    };
    let echo_agent_auth = match auth.auth().await {
        Ok(a) => a,
        Err(err) => {
            tracing::warn!(error = %err, "product skills: auth unavailable");
            return None;
        }
    };
    if let Some(skills) = product_skills_cache_lookup(&echo_agent_auth) {
        return skills;
    }
    let _gate = product_skills_fetch_gate().lock().await;
    if let Some(skills) = product_skills_cache_lookup(&echo_agent_auth) {
        return skills;
    }
    let client = crate::remote::SkillsClient::new(auth);
    match client.try_list_catalog(PRODUCT_SKILLS_LOCALE).await {
        Ok((catalog, used_untagged_recovery)) if catalog.user_list_failed => {
            let skills = catalog.to_skill_infos();
            {
                let guard = PRODUCT_SKILLS_CACHE.lock();
                if let Some(entry) = guard.as_ref()
                    && product_skills_cache_matches(entry, &echo_agent_auth)
                {
                    tracing::warn!(
                        skill_count = entry.skills.len(),
                        "product skills: user list failed — reusing last successful catalog"
                    );
                    return Some(entry.skills.clone());
                }
            }
            *PRODUCT_SKILLS_DEGRADED_CACHE.lock() = Some(product_skills_cache_entry_after_fetch(
                &echo_agent_auth,
                skills.clone(),
                used_untagged_recovery,
            ));
            clear_negative_cache_for_auth(&echo_agent_auth);
            tracing::warn!(
                skill_count = skills.len(),
                used_untagged_recovery,
                "product skills: user list failed — caching degraded catalog briefly"
            );
            Some(skills)
        }
        Ok((catalog, used_untagged_recovery)) => {
            let skills = catalog.to_skill_infos();
            *PRODUCT_SKILLS_CACHE.lock() = Some(product_skills_cache_entry_after_fetch(
                &echo_agent_auth,
                skills.clone(),
                used_untagged_recovery,
            ));
            clear_degraded_cache_for_auth(&echo_agent_auth);
            clear_negative_cache_for_auth(&echo_agent_auth);
            Some(skills)
        }
        Err(err) => {
            let cached = PRODUCT_SKILLS_CACHE.lock().clone();
            if let Some(entry) = cached
                && product_skills_cache_matches(&entry, &echo_agent_auth)
            {
                tracing::warn!(
                    error = %err,
                    skill_count = entry.skills.len(),
                    "product skills: catalog unavailable — reusing last successful catalog"
                );
                return Some(entry.skills);
            }
            *PRODUCT_SKILLS_NEGATIVE_CACHE.lock() =
                Some(product_skills_negative_stamp(&echo_agent_auth));
            tracing::warn!(
                error = %err,
                "product skills: catalog unavailable after retries — negative cache"
            );
            None
        }
    }
}
/// `Some(Some(skills))` success/degraded hit, `Some(None)` negative hit, `None` miss.
fn product_skills_cache_lookup(
    echo_agent_auth: &crate::auth::EchoAgentAuth,
) -> Option<Option<Vec<SkillInfo>>> {
    {
        let guard = PRODUCT_SKILLS_CACHE.lock();
        if let Some(entry) = guard.as_ref()
            && product_skills_cache_matches(entry, echo_agent_auth)
            && entry.fetched_at.elapsed() < PRODUCT_SKILLS_SUCCESS_TTL
        {
            return Some(Some(entry.skills.clone()));
        }
    }
    {
        let guard = PRODUCT_SKILLS_DEGRADED_CACHE.lock();
        if let Some(entry) = guard.as_ref()
            && product_skills_cache_matches(entry, echo_agent_auth)
            && entry.fetched_at.elapsed() < PRODUCT_SKILLS_DEGRADED_TTL
        {
            return Some(Some(entry.skills.clone()));
        }
    }
    {
        let guard = PRODUCT_SKILLS_NEGATIVE_CACHE.lock();
        if let Some(entry) = guard.as_ref()
            && product_skills_negative_matches(entry, echo_agent_auth)
            && entry.fetched_at.elapsed() < PRODUCT_SKILLS_NEGATIVE_TTL
        {
            return Some(None);
        }
    }
    None
}
/// Skill source for `available_commands_update` given sticky session kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AcuSkillSource {
    /// Product REST catalog (`kind: chat`).
    Product,
    /// Disk / plugin skills via `SkillManager` (Build).
    Disk,
}
pub(crate) fn acu_skill_source(is_chat_kind: bool) -> AcuSkillSource {
    if is_chat_kind {
        AcuSkillSource::Product
    } else {
        AcuSkillSource::Disk
    }
}
/// Build the available commands list, optionally scoped to a working directory.
/// - `Some(cwd)`: full skill discovery (Local + Repo + User) + builtins.
/// - `None`: builtins + global (User-scoped) skills only.
/// - `kind == Some("chat")` (feature `chat` only): **product Skills REST
///   catalog** (same as echo-agent-web) + builtins — not Build disk skills. Without
///   the feature, returns `Err` (invalid params). Product REST failure still
///   advertises builtins only (empty product skills).
pub(crate) async fn list_commands(
    cwd: Option<&str>,
    skills_config: &echo_agent_core::prompt::skills::SkillsConfig,
    plugin_registry: Option<&echo_agent_core::plugins::PluginRegistry>,
    availability: CommandAvailability,
    compat: echo_agent_tools::types::compat::CompatConfig,
    include_project_workflows: bool,
    kind: Option<&str>,
    auth: Option<std::sync::Arc<crate::auth::AuthManager>>,
) -> Result<ListCommandsResponse, acp::Error> {
    if kind == Some("chat") {
        {
            return Err(acp::Error::invalid_params()
                .data("commands/list kind=\"chat\" requires a chat-enabled binary"));
        }
        let skills = product_skill_infos(auth).await.unwrap_or_default();
        return Ok(ListCommandsResponse {
            commands: available_commands(&skills, availability, &[]),
            tools: None,
        });
    }
    let skills = echo_agent_core::prompt::skills::list_skills_with_plugins(
        cwd,
        skills_config,
        plugin_registry,
        compat,
    )
    .await;
    let workflows = crate::session::workflow::registry::list_workflows(
        include_project_workflows
            .then_some(cwd)
            .flatten()
            .map(std::path::Path::new),
    );
    Ok(ListCommandsResponse {
        commands: available_commands(&skills, availability, &workflows),
        tools: None,
    })
}
/// A parsed skill reference from user input.
///
/// Produced by `parse_skill_references()` when scanning user text for known
/// `/{skill_name}` tokens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ParsedSkillRef {
    /// The skill name (bare or qualified, as typed by the user).
    pub name: String,
    /// Arguments following this skill reference, up to the next skill or end-of-input.
    pub args: String,
    /// The resolved `SkillInfo` path, for loading SKILL.md.
    pub skill_path: String,
    /// Scope-qualified name (e.g. "user:commit"), used for telemetry.
    pub qualified_name: String,
    /// Plugin name if this is a plugin skill.
    pub plugin_name: Option<String>,
}
#[derive(Debug)]
pub(super) enum SlashCommandOutcome {
    /// Execute directly, no model round-trip.
    Builtin(BuiltinAction),
    /// One or more skills detected in user input.
    ///
    /// The original prompt `blocks` are preserved verbatim — they are NOT
    /// rewritten. The shell's prompt assembly layer will read each skill's
    /// SKILL.md, apply substitutions, and build the `<skill_information>`
    /// envelope alongside the `<user_query>` block.
    InvokeSkill {
        /// The original, unmodified prompt blocks.
        blocks: Vec<acp::ContentBlock>,
        /// Parsed skill references (one per detected `/{skill}` token).
        skills: Vec<ParsedSkillRef>,
    },
}
#[derive(Debug)]
pub(super) enum BuiltinAction {
    Compact {
        user_context: Option<String>,
    },
    SetYolo {
        enabled: bool,
    },
    FlushMemory,
    Dream,
    ContextInfo,
    HooksTrust,
    HooksList,
    HooksAdd {
        path: String,
    },
    HooksRemove {
        path: String,
    },
    HooksUntrust,
    PluginsList,
    PluginsReload,
    PluginsTrust,
    SessionInfo,
    PluginsAdd {
        path: String,
    },
    PluginsRemove {
        path: String,
    },
    PluginsInstall {
        source: String,
        trust: bool,
    },
    PluginsUninstall {
        name: String,
        confirm: bool,
    },
    PluginsUpdate {
        name: Option<String>,
    },
    Feedback {
        text: String,
    },
    MemoryBrowse,
    MemoryToggle {
        enabled: bool,
    },
    GoalSet {
        objective: String,
        token_budget: Option<i64>,
    },
    GoalStatus,
    GoalPause,
    GoalResume,
    GoalClear,
    DeepResearch {
        query: String,
    },
    WorkflowManage {
        run_id: String,
        op: String,
    },
    WorkflowLaunch {
        name: String,
        input: String,
    },
}
impl BuiltinAction {
    pub(crate) fn command_name(&self) -> &'static str {
        match self {
            BuiltinAction::Compact { .. } => "compact",
            BuiltinAction::SetYolo { .. } => "yolo",
            BuiltinAction::FlushMemory => "flush",
            BuiltinAction::Dream => "dream",
            BuiltinAction::ContextInfo => "context",
            BuiltinAction::HooksTrust => "hooks-trust",
            BuiltinAction::HooksList => "hooks-list",
            BuiltinAction::HooksAdd { .. } => "hooks-add",
            BuiltinAction::HooksRemove { .. } => "hooks-remove",
            BuiltinAction::HooksUntrust => "hooks-untrust",
            BuiltinAction::PluginsList => "plugins-list",
            BuiltinAction::PluginsReload => "plugins-reload",
            BuiltinAction::PluginsTrust => "plugins-trust",
            BuiltinAction::SessionInfo => "session",
            BuiltinAction::PluginsAdd { .. } => "plugins-add",
            BuiltinAction::PluginsRemove { .. } => "plugins-remove",
            BuiltinAction::PluginsInstall { .. } => "plugins-install",
            BuiltinAction::PluginsUninstall { .. } => "plugins-uninstall",
            BuiltinAction::PluginsUpdate { .. } => "plugins-update",
            BuiltinAction::Feedback { .. } => "feedback",
            BuiltinAction::MemoryBrowse => "memory",
            BuiltinAction::MemoryToggle { .. } => "memory",
            BuiltinAction::GoalSet { .. }
            | BuiltinAction::GoalStatus
            | BuiltinAction::GoalPause
            | BuiltinAction::GoalResume
            | BuiltinAction::GoalClear => "goal",
            BuiltinAction::DeepResearch { .. } => "deep-research",
            BuiltinAction::WorkflowManage { .. } => "workflow",
            BuiltinAction::WorkflowLaunch { .. } => "workflow",
        }
    }
    pub(crate) fn args_provided(&self) -> bool {
        match self {
            BuiltinAction::Compact { user_context } => user_context.is_some(),
            BuiltinAction::SetYolo { .. } => true,
            BuiltinAction::FlushMemory => false,
            BuiltinAction::Dream => false,
            BuiltinAction::ContextInfo => false,
            BuiltinAction::HooksTrust => false,
            BuiltinAction::HooksList => false,
            BuiltinAction::HooksAdd { .. } => true,
            BuiltinAction::HooksRemove { .. } => true,
            BuiltinAction::HooksUntrust => false,
            BuiltinAction::PluginsList => false,
            BuiltinAction::PluginsReload => false,
            BuiltinAction::PluginsTrust => false,
            BuiltinAction::SessionInfo => false,
            BuiltinAction::PluginsAdd { .. } => true,
            BuiltinAction::PluginsRemove { .. } => true,
            BuiltinAction::PluginsInstall { .. } => true,
            BuiltinAction::PluginsUninstall { .. } => true,
            BuiltinAction::PluginsUpdate { name } => name.is_some(),
            BuiltinAction::Feedback { text } => !text.is_empty(),
            BuiltinAction::MemoryBrowse => false,
            BuiltinAction::MemoryToggle { .. } => true,
            BuiltinAction::GoalSet { .. } => true,
            BuiltinAction::GoalStatus
            | BuiltinAction::GoalPause
            | BuiltinAction::GoalResume
            | BuiltinAction::GoalClear => false,
            BuiltinAction::DeepResearch { .. } => true,
            BuiltinAction::WorkflowManage { .. } => true,
            BuiltinAction::WorkflowLaunch { input, .. } => !input.is_empty(),
        }
    }
}
/// How to rewrite the user's prompt when a slash command resolves to a skill.
///
/// - `RewriteToRun` (default): replace `/foo args` with `"run /foo args"`,
///   matching today's EchoAgent Build flow that calls our dedicated `skill` tool.
/// - `Passthrough`: leave the prompt verbatim. Some templates use this —
///   the model is trained to spot a leading `/<name>`, look it up in the
///   `<agent_skills>` listing, and call the Read tool on `fullPath`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum SkillSlashRewrite {
    #[default]
    RewriteToRun,
    Passthrough,
}
/// Scan user input left-to-right for `/{word}` tokens where `word` matches
/// a **known registered skill name** (bare or qualified).
///
/// Unknown `/words` (like `/api/v2/users`, `/tmp/file`) are NOT treated as
/// skill references — only tokens that resolve to a known skill count.
///
/// Returns `None` when no known skill references are found. Otherwise returns
/// the list of `ParsedSkillRef` entries with each skill's args (the text
/// between one skill token and the next, or end-of-input).
pub(crate) fn parse_skill_references(
    text: &str,
    skills: &[SkillInfo],
    availability: CommandAvailability,
) -> Option<Vec<ParsedSkillRef>> {
    let catalog = EffectiveCommandCatalog::build(skills, availability, &[]);
    parse_skill_references_with_catalog(text, &catalog)
}
fn parse_skill_references_with_catalog(
    text: &str,
    catalog: &EffectiveCommandCatalog<'_>,
) -> Option<Vec<ParsedSkillRef>> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    struct SkillHit<'a> {
        offset: usize,
        typed_name: String,
        skill: &'a SkillInfo,
    }
    let mut hits = Vec::new();
    let bytes = trimmed.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'/' {
            i += 1;
            continue;
        }
        if i > 0 && !bytes[i - 1].is_ascii_whitespace() {
            i += 1;
            continue;
        }
        let start = i + 1;
        if start >= bytes.len() {
            break;
        }
        let end = trimmed[start..]
            .find(|c: char| c.is_whitespace())
            .map(|relative| start + relative)
            .unwrap_or(trimmed.len());
        let word = &trimmed[start..end];
        let hit = if i == 0 {
            catalog.skill_resolvable(word)
        } else {
            catalog.skill(word)
        };
        if let Some(skill) = hit {
            hits.push(SkillHit {
                offset: i,
                typed_name: word.to_string(),
                skill,
            });
        }
        i = end.max(start);
    }
    if hits.is_empty() {
        return None;
    }
    Some(
        hits.iter()
            .enumerate()
            .map(|(index, hit)| {
                let word_end = hit.offset + 1 + hit.typed_name.len();
                let args_end = hits
                    .get(index + 1)
                    .map(|next| next.offset)
                    .unwrap_or(trimmed.len());
                ParsedSkillRef {
                    name: hit.typed_name.clone(),
                    args: trimmed[word_end..args_end].trim().to_string(),
                    skill_path: hit.skill.path.clone(),
                    qualified_name: format_skill_name(hit.skill),
                    plugin_name: hit.skill.plugin_name.clone(),
                }
            })
            .collect(),
    )
}
/// Load each parsed skill's SKILL.md, apply substitutions, and build the
/// `<skill_information>` envelope.
///
/// Shared by turn start (prompt assembly in `process_conversation_turn`) and
/// the mid-turn interjection drain, so a skill delivers identically whether
/// it starts a turn or is force-sent into a running one. Returns `None` when
/// no skill content loads (missing files are logged and skipped; the
/// `<skills_referenced>` index still lists every parsed ref).
pub(super) async fn build_skill_information_for_refs(
    parsed_skills: &[ParsedSkillRef],
    slash_skills: &[SkillInfo],
    session_id: &str,
) -> Option<String> {
    use echo_agent_tools::implementations::skills::skill::{
        SkillRef, SubstitutionContext, apply_substitutions, build_skill_block,
        build_skill_information, load_skill_content,
    };
    let mut skill_blocks: Vec<String> = Vec::new();
    for sk in parsed_skills {
        let Some(info) = slash_skills.iter().find(|s| s.path == sk.skill_path) else {
            continue;
        };
        match load_skill_content(info).await {
            Ok(mut content) => {
                let skill_dir = std::path::Path::new(&info.path)
                    .parent()
                    .and_then(|p| p.to_str());
                let args = if sk.args.is_empty() {
                    None
                } else {
                    Some(sk.args.as_str())
                };
                apply_substitutions(
                    &mut content,
                    args,
                    &SubstitutionContext {
                        skill_dir,
                        session_id: Some(session_id),
                        plugin_root: info.plugin_root.as_deref(),
                        plugin_data: info.plugin_data.as_deref(),
                    },
                );
                skill_blocks.push(build_skill_block(&sk.name, &sk.args, &content));
            }
            Err(e) => {
                let body_less_product =
                    info.path.contains("://") && info.body.as_ref().is_none_or(|b| b.is_empty());
                if body_less_product {
                    tracing::debug!(
                        skill = %sk.name,
                        path = %info.path,
                        "product skill has no local body — skip shell expansion"
                    );
                } else {
                    tracing::warn!(
                        skill = %sk.name,
                        error = %e,
                        "failed to load skill for expansion"
                    );
                }
            }
        }
    }
    if skill_blocks.is_empty() {
        return None;
    }
    let refs: Vec<SkillRef<'_>> = parsed_skills
        .iter()
        .map(|sk| SkillRef {
            name: &sk.name,
            path: &sk.skill_path,
        })
        .collect();
    Some(build_skill_information(&skill_blocks, &refs))
}
/// Resolve prompt blocks as a slash command.
/// `Ok(blocks)` = not a command, pass through. `Err(outcome)` = matched.
pub(super) fn resolve(
    prompt_blocks: Vec<acp::ContentBlock>,
    skills: &[SkillInfo],
    availability: CommandAvailability,
    _skill_rewrite: SkillSlashRewrite,
    workflows: &[crate::session::workflow::registry::WorkflowListing],
    loop_fire_mode: LoopFireMode,
) -> Result<Vec<acp::ContentBlock>, SlashCommandOutcome> {
    let Some((command_name, args)) = parse_slash_prefix(&prompt_blocks) else {
        return Ok(prompt_blocks);
    };
    let command_key = slash_key(command_name);
    if let Some(prompt_cmd) = PROMPT_COMMANDS
        .iter()
        .find(|c| slash_key(c.name) == command_key)
        && availability.allows(prompt_cmd.gate)
    {
        let mut blocks = match prompt_cmd.name {
            "loop" => build_loop_prompt_blocks(args, loop_fire_mode),
            other => {
                unreachable!("prompt-only command /{other} has no resolver wired in resolve()")
            }
        };
        let display_text = if args.is_empty() {
            format!("/{command_name}")
        } else {
            format!("/{command_name} {args}")
        };
        if let Some(acp::ContentBlock::Text(tb)) = blocks.first_mut() {
            let map = tb.meta.get_or_insert_with(acp::Meta::new);
            map.insert(
                "displayText".to_string(),
                serde_json::Value::String(display_text),
            );
        }
        return Err(SlashCommandOutcome::InvokeSkill {
            blocks,
            skills: vec![],
        });
    }
    let catalog = EffectiveCommandCatalog::build(skills, availability, workflows);
    if let Some(builtin) = catalog.builtins.iter().find(|builtin| {
        slash_key(builtin.name) == command_key
            || builtin
                .aliases
                .iter()
                .any(|alias| slash_key(alias) == command_key)
    }) {
        let action = (builtin.resolve)(args);
        if matches!(action, BuiltinAction::WorkflowLaunch { .. }) && !availability.workflows {
            return Ok(prompt_blocks);
        }
        return Err(SlashCommandOutcome::Builtin(action));
    }
    let full_text = prompt_blocks
        .iter()
        .find_map(|b| {
            if let acp::ContentBlock::Text(t) = b {
                Some(t.text.as_str())
            } else {
                None
            }
        })
        .unwrap_or("");
    if let Some(parsed_skills) = parse_skill_references_with_catalog(full_text, &catalog) {
        return Err(SlashCommandOutcome::InvokeSkill {
            blocks: prompt_blocks,
            skills: parsed_skills,
        });
    }
    if let Some(workflow) = catalog.workflow(&command_key) {
        return Err(SlashCommandOutcome::Builtin(
            BuiltinAction::WorkflowLaunch {
                name: workflow.name.clone(),
                input: args.to_string(),
            },
        ));
    }
    Ok(prompt_blocks)
}
/// Extract `(name, args)` if the first text block starts with `/`.
///
/// - `"/compact keep auth"` → `Some(("compact", "keep auth"))`
/// - `"please run /commit"` → `None` (not at start)
fn parse_slash_prefix(prompt_blocks: &[acp::ContentBlock]) -> Option<(&str, &str)> {
    let text = prompt_blocks.iter().find_map(|b| {
        if let acp::ContentBlock::Text(t) = b {
            Some(t.text.as_str())
        } else {
            None
        }
    })?;
    let trimmed = text.trim();
    let without_slash = trimmed.strip_prefix('/')?;
    let (name, args) = match without_slash.find(char::is_whitespace) {
        Some(idx) => (&without_slash[..idx], without_slash[idx..].trim()),
        None => (without_slash, ""),
    };
    if name.is_empty() {
        return None;
    }
    Some((name, args))
}
/// Build the `/loop` prompt blocks for the shell client.
///
/// The wording (usage hint + scheduling instruction) is sourced from
/// `echo-agent-tools` so it stays identical to the pager's `LoopCommand` and the
/// two front-ends can't drift. Like the pager, there is no host-side interval
/// default: the model derives the cadence from the request and asks when none
/// is given.
fn build_loop_prompt_blocks(args: &str, mode: LoopFireMode) -> Vec<acp::ContentBlock> {
    use echo_agent_tools::implementations::echo_agent_build::{
        loop_schedule_instruction, loop_usage_message,
    };
    let text = if args.trim().is_empty() {
        loop_usage_message().to_string()
    } else {
        loop_schedule_instruction(args, mode)
    };
    vec![acp::ContentBlock::Text(acp::TextContent::new(text))]
}
#[cfg(test)]
#[path = "slash_commands_tests.rs"]
mod tests;
