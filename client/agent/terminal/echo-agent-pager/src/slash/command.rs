//! Slash command trait and execution types.
//!
//! Pager's synchronous dispatch model. Key differences:
//!
//! - `run()` is synchronous (no `async_trait`). Commands that need async work
//!   return `CommandResult::Action(action)` and let the dispatch layer handle it.
//! - `CommandResult` has additional variants: `Action`, `QueueCommand`, `PassThrough`.
//! - Trait methods return `&str` (not `&'static str`) to support ACP-sourced commands.
//! - `args_required()` added for the two-bit completeness model.
//! - `validate_args()` intentionally omitted in phase 1 (folded into `run()`).

use crate::acp::model_state::ModelState;
use crate::app::actions::Action;
use crate::app::bundle::BundleState;
use crate::slash::mode_support::ModeSupport;
use agent_client_protocol as acp;

/// Provisional scheduled task info for immediate display in the tasks pane.
///
/// Created by `/loop` when the user submits the command so the task appears
/// instantly, rather than waiting for the LLM round-trip through
/// `scheduler_create`.
#[derive(Debug, Clone)]
pub struct ScheduledTaskPreview {
    pub prompt: String,
    pub human_schedule: String,
    pub next_fire_at: Option<String>,
    /// Tag shown in the tasks pane (e.g. "loop", "check"). Defaults to "loop".
    pub tag: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DoctorRequest {
    Report,
    ListFixes,
    Fix(crate::diagnostics::DiagnosticId),
}

/// Result of running a slash command.
#[derive(Debug)]
#[allow(clippy::large_enum_variant)]
pub enum CommandResult {
    /// Command handled successfully, no visible output needed.
    /// Included for TUI parity; no phase-1 command uses this directly.
    Handled,
    /// Command handled but was a no-op (e.g., model already selected).
    /// Included for TUI parity. Dispatch treats it identically to Handled.
    HandledNoOp,
    /// Build or act on TUI doctor state from live app/session inputs.
    Doctor(DoctorRequest),
    /// Command failed with an error message.
    Error(String),
    /// Command produced a user-visible message.
    Message(String),
    /// Command produced a pager Action to dispatch (e.g., SwitchModel, Quit).
    Action(Action),
    /// Command should be sent through the queued command pipeline
    /// (e.g., /compact). The String is the raw command text.
    QueueCommand(String),
    /// Skill invocation: pager read the SKILL.md, applied substitutions,
    /// and constructed structured prompt blocks for the wire.
    /// `display_text` is what the user sees in scrollback.
    /// `prompt_blocks` is the actual content sent to the model.
    InjectSkill {
        display_text: String,
        prompt_blocks: Vec<agent_client_protocol::ContentBlock>,
        /// Whether to display as a skill invocation (teal accent) in scrollback.
        /// `true` for real skills (e.g. /commit), `false` for built-in commands
        /// like /loop that inject structured prompts but aren't skills.
        display_as_skill: bool,
        /// If set, immediately show a provisional scheduled task in the tasks
        /// pane (replaced when the real `ScheduledTaskCreated` notification
        /// arrives from the shell).
        scheduled_task_preview: Option<ScheduledTaskPreview>,
    },
    /// Command text should be sent as a regular prompt. The shell resolves it.
    ///
    /// Phase-1 simplification: this intentionally covers two semantically
    /// different cases in a single variant:
    /// 1. ACP-advertised commands (shell explicitly supports them)
    /// 2. Unknown commands (pager doesn't know them, shell might)
    ///
    /// Both are sent identically today. If behavior ever needs to diverge
    /// (e.g., different UX confidence, error messaging, or telemetry),
    /// split into `AcpPassThrough` and `UnknownPassThrough` variants.
    PassThrough(String),
}

/// A suggestion item for command argument completion.
#[derive(Debug, Clone)]
pub struct ArgItem {
    /// Display text shown in the dropdown.
    pub display: String,
    /// Text used for fuzzy matching.
    pub match_text: String,
    /// Text inserted into the prompt on acceptance.
    pub insert_text: String,
    /// Description shown alongside the item.
    pub description: String,
}

/// A saved or built-in workflow the `/workflow` picker can launch, sourced
/// from ACP commands that carry `_meta.workflowSource`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowChoice {
    pub name: String,
    pub description: String,
}

/// A session workflow run the `/workflow` manage verbs can target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowRunChoice {
    pub name: String,
    pub status: String,
    pub builtin: bool,
}

impl WorkflowRunChoice {
    pub fn can_pause(&self) -> bool {
        self.status == "active"
    }

    pub fn can_resume(&self) -> bool {
        // Picker: only runs the user stopped (`/workflow stop`) or paused
        // (`/workflow pause`). System pauses (blocked / back-off / budget)
        // stay off this list; they are still resumable if typed by name.
        matches!(self.status.as_str(), "user_paused" | "cancelled")
    }

    pub fn can_stop(&self) -> bool {
        !matches!(
            self.status.as_str(),
            "interrupted" | "complete" | "failed" | "cancelled"
        )
    }

    pub fn can_save(&self, definitions: &[WorkflowChoice]) -> bool {
        // Shell save requires display name == script `meta.name`. First
        // runs keep the catalog name; uniquified copies (`review-pr-2`)
        // do not. A definition literally named `sprint-2` is still
        // savable because that name is in the catalog.
        !self.builtin
            && definitions
                .iter()
                .any(|workflow| workflow.name == self.name)
    }
}

impl WorkflowChoice {
    /// `None` when the command is not a workflow definition.
    pub fn from_acp(cmd: &acp::AvailableCommand) -> Option<Self> {
        cmd.meta.as_ref()?.get("workflowSource")?;
        let description = cmd
            .description
            .strip_prefix("Workflow: ")
            .unwrap_or(&cmd.description)
            .to_string();
        Some(Self {
            name: cmd.name.clone(),
            description,
        })
    }
}

/// Read-only context for generating suggestions.
///
/// Passed to `SlashCommand::suggest_args()` and `SlashCommand::visible()`.
/// Kept minimal -- extend as needed.
pub struct AppCtx<'a> {
    pub models: &'a ModelState,
    /// Working directory of the active session (for filesystem completions).
    pub cwd: &'a std::path::Path,
    /// Session announcements (critical or promo) exist (gates `/announcements` visibility).
    pub has_session_announcements: bool,
    /// Consumer billing surface (`AppView::usage_visible`). Gates `/usage` subcommands.
    pub billing_surface_visible: bool,
    /// Whether `/usage` is offered and executable. False for external-auth
    /// deployments with no cloud.echo-agent.invalid billing session.
    pub usage_command_visible: bool,
    pub workflows_available: bool,
    /// Saved / built-in workflow definitions advertised by the shell
    /// (`_meta.workflowSource`). Backs `/workflow` argument suggestions.
    pub saved_workflows: &'a [WorkflowChoice],
    /// Live session runs. Backs `/workflow pause|resume|stop|save` name
    /// suggestions so a manage verb never auto-picks a run.
    pub workflow_runs: &'a [WorkflowRunChoice],
    /// Effective render mode of this process (gates `/minimal` and
    /// `/fullscreen` visibility). Same source of truth as
    /// [`CommandExecCtx::screen_mode`], carried by the owning
    /// [`SlashController`](crate::slash::SlashController).
    pub(crate) screen_mode: crate::app::ScreenMode,
    /// Current session title for `/rename` ghost-prefill (`display_name`,
    /// else `generated_session_title`). `None` when there is no title yet.
    pub current_title: Option<&'a str>,
}

/// Mutable execution context for `SlashCommand::run()`.
///
/// Wraps only what pager can cleanly provide. Commands that need async ACP
/// calls return `CommandResult::Action(...)` and let dispatch handle the effect.
pub struct CommandExecCtx<'a> {
    pub models: &'a ModelState,
    pub session_id: Option<&'a acp::SessionId>,
    pub bundle_state: &'a BundleState,
    pub(crate) screen_mode: crate::app::ScreenMode,
    /// Consumer billing surface (`AppView::usage_visible`). Gates `/usage` subcommands.
    pub billing_surface_visible: bool,
    /// Whether `/usage` is offered and executable. False for external-auth
    /// deployments with no cloud.echo-agent.invalid billing session.
    pub usage_command_visible: bool,
    /// Snapshot of the active agent's PAGER-owned settings, built at
    /// command-build time by the dispatcher. Slash commands like
    /// `/multiline` read this to compute `!current` and dispatch a
    /// typed `Action::SetX(new)` — the dispatcher remains the single
    /// source of truth for the actual state mutation.
    pub(crate) pager_state: crate::settings::PagerLocalSnapshot,
}

/// Origin of a slash command. The dropdown renderer turns this into badge
/// text via [`CommandProvenance::badge`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandProvenance {
    Builtin,
    /// Non-skill ACP command (e.g. `/flush`).
    Shell,
    /// Skill; `source` is the plugin install name or scope.
    Skill {
        source: String,
    },
}

impl CommandProvenance {
    /// Right-aligned slash-menu badge. Shell commands render as `built-in`
    /// too — the badge only has to separate a skill from whoever kept the
    /// bare name.
    pub fn badge(&self) -> std::borrow::Cow<'static, str> {
        match self {
            Self::Builtin | Self::Shell => std::borrow::Cow::Borrowed("built-in"),
            Self::Skill { source } => std::borrow::Cow::Owned(format!("skill · {source}")),
        }
    }
}

/// A slash command.
///
/// Implementors define command metadata (name, description, args) and
/// synchronous execution logic. The trait uses `&str` returns (not
/// `&'static str`) so ACP-sourced commands with runtime-determined data
/// work from day one.
pub trait SlashCommand: Send + Sync {
    /// Canonical command name (without leading `/`). E.g., `"exit"`.
    fn name(&self) -> &str;

    /// Alternative names for this command. E.g., `&["quit"]` for `/exit`.
    fn aliases(&self) -> &[&str] {
        &[]
    }

    /// Short human-readable description shown in the dropdown.
    fn description(&self) -> &str;

    /// Origin for the slash-menu provenance badge. Defaults to builtin.
    fn provenance(&self) -> CommandProvenance {
        CommandProvenance::Builtin
    }

    /// Usage string shown in help. E.g., `"/model <name>"`.
    fn usage(&self) -> &str;

    /// Whether the command accepts arguments at all.
    fn takes_args(&self) -> bool {
        false
    }

    /// Runtime args contract (e.g. subcommands only for some auth modes).
    /// Defaults to [`Self::takes_args`]. Dropdown/completion paths only:
    /// insert text (trailing space), the args-phase snapshot, and argument
    /// suggestions. Enter-completeness ([`crate::slash::is_command_complete`])
    /// keys off the static [`Self::takes_args`] / [`Self::args_required`] pair.
    #[allow(unused_variables)]
    fn takes_args_now(&self, ctx: &AppCtx) -> bool {
        self.takes_args()
    }

    /// Whether arguments are required for execution.
    ///
    /// Only meaningful when `takes_args()` is true. The two-bit model:
    ///
    /// | `takes_args` | `args_required` | Example          | Enter with no args |
    /// |-------------|----------------|------------------|-------------------|
    /// | `false`     | `false`        | `/exit`          | Executes          |
    /// | `true`      | `false`        | `/compact [ctx]` | Executes          |
    /// | `true`      | `true`         | `/model <id>`    | Blocks            |
    fn args_required(&self) -> bool {
        false
    }

    /// Generate argument suggestions. `args_query` is the raw typed
    /// args text; most impls ignore it and return a static list.
    #[allow(unused_variables)]
    fn suggest_args(&self, ctx: &AppCtx, args_query: &str) -> Option<Vec<ArgItem>> {
        None
    }

    /// Whether this command is currently visible / executable.
    ///
    /// Default is `true` (every command is visible). Override to gate a
    /// command on session state.
    #[allow(unused_variables)]
    fn visible(&self, ctx: &AppCtx) -> bool {
        true
    }

    /// Whether this command operates on a single agent session — its
    /// conversation, context, model, turns, plan, etc. — rather than the
    /// pager as a whole.
    ///
    /// Session-scoped commands (`/compact`, `/fork`, `/rewind`, …) need a
    /// "current session" to act on, so they are suppressed on session-less
    /// surfaces. Today that means the agent dashboard's dispatch input,
    /// which offers only pager-global commands (`/theme`, `/settings`,
    /// `/mcps`, …). Surfaces that always have a session (the agent view)
    /// ignore this flag and continue to show every command.
    ///
    /// Defaults to `false` (pager-global).
    fn session_scoped(&self) -> bool {
        false
    }

    /// Whether a `session_scoped()` command should still be offered on
    /// session-less surfaces (the agent dashboard's dispatch input).
    ///
    /// A handful of session-scoped commands have a meaningful session-less
    /// interpretation: `/model` and `/plan` configure the *next* agent the
    /// dashboard spawns; `/multiline` toggles compose mode on the dashboard
    /// inputs. Those override this to `true` so they appear in the dashboard
    /// dropdown even though `session_scoped()` is `true`. Has no effect for
    /// non-session-scoped commands (they're always offered).
    ///
    /// Defaults to `false`.
    fn offered_when_session_less(&self) -> bool {
        false
    }

    /// Whether this command should ONLY be offered on the session-less
    /// dashboard surface — the inverse of [`Self::session_scoped`]. The
    /// dashboard's dispatch input is the one surface where
    /// `hide_session_scoped` is set, so a `dashboard_only` command shows
    /// there and is suppressed on every session surface (the agent view) and
    /// the welcome screen.
    ///
    /// `/cd` changes where the dashboard dispatches new agents, so it is
    /// meaningless in an agent session and hidden there. Defaults to `false`.
    fn dashboard_only(&self) -> bool {
        false
    }

    /// Which render modes this command functions in.
    ///
    /// Minimal mode (`echoagent --minimal`) deletes the interactive fullscreen
    /// scrollback pane, the in-app mouse selection path, and the agent
    /// dashboard, handing scroll / search / selection back to the terminal
    /// (K7); a few commands exist only there, because the full TUI solves the
    /// same problem with a pane or a chord. Declaring the mode here is the
    /// single source for both behaviors: the command is hidden from every
    /// completion surface in the modes it does not support (`command_offered`),
    /// and a fully-typed invocation is refused with an actionable hint by the
    /// central dispatch gate instead of running against a surface that does not
    /// exist.
    ///
    /// Defaults to [`ModeSupport::Both`] — a **denylist, not an allowlist**:
    /// the many mode-agnostic commands keep working and new commands are
    /// available everywhere by default (minimal is converging toward parity).
    /// Clipboard helpers like `/copy` stay `Both`: they read scrollback state
    /// and do not need the fullscreen pane.
    fn mode_support(&self) -> ModeSupport {
        ModeSupport::Both
    }

    /// Placeholder text shown in the prompt when args are empty.
    /// E.g., `"[context]"` for `/compact`.
    fn arg_placeholder(&self) -> Option<&str> {
        None
    }

    /// Whether this command is a skill (ACP-advertised with skill metadata).
    /// Used for visual theming (accent color, prefix glyph).
    fn is_skill(&self) -> bool {
        false
    }

    /// Tool names the agent must have registered for this command to work.
    ///
    /// Default is empty (no tool dependency). Override for commands that
    /// only make sense when specific tools are available -- e.g. `/loop`
    /// requires `scheduler_create`. The registry hides commands whose
    /// requirements aren't all present in the agent's advertised toolset.
    fn required_tools(&self) -> &[&str] {
        &[]
    }

    /// Whether this command supports live preview when navigating arg
    /// suggestions in the dropdown.
    ///
    /// When true, [`preview_arg`] is called on every selection change
    /// and [`cancel_preview`] on dropdown close (Esc).
    fn supports_preview(&self) -> bool {
        false
    }

    /// Capture the current preview-relevant state as a string.
    ///
    /// Called once when preview mode begins (first navigation in args
    /// dropdown). The returned value is stored and passed back to
    /// [`cancel_preview`] if the user dismisses the dropdown.
    fn preview_state(&self) -> Option<String> {
        None
    }

    /// Live-preview the given argument suggestion.
    ///
    /// Called when the user navigates to a new suggestion in the dropdown
    /// (Up/Down). The command should apply a temporary/preview state.
    /// Only called when [`supports_preview`] returns true.
    #[allow(unused_variables)]
    fn preview_arg(&self, arg: &str) {}

    /// Cancel a live preview, reverting to the state before the dropdown
    /// opened. `previous` is the value returned by [`preview_state`]
    /// when preview started.
    ///
    /// Called when the user dismisses the dropdown (Esc) or clears the
    /// slash input. Only called when [`supports_preview`] returns true.
    #[allow(unused_variables)]
    fn cancel_preview(&self, previous: &str) {}

    /// Execute the command synchronously.
    ///
    /// For async work, return `CommandResult::Action(action)` and let
    /// the dispatch layer handle the effect pipeline.
    fn run(&self, ctx: &mut CommandExecCtx, args: &str) -> CommandResult;
}
