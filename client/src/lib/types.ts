/**
 * ACP (Agent Client Protocol) wire types — TypeScript mirror of the subset
 * of `agent-client-protocol` messages EchoAgent's Rust backend forwards to
 * the frontend as Tauri events.
 *
 * Source of truth: the `agent-client-protocol` 0.10.4 crate (used by EchoAgent)
 * and the echo.agent extensions documented in
 *   echo-agent-build/crates/codegen/echo-agent-runtime/src/extensions/notification.rs
 *
 * The Rust backend serializes these with serde and emits them as the `payload`
 * of `agent://update` / `agent://permission` / `agent://complete` events.
 */

// ---------- content blocks ----------

export interface TextContent {
  type: "text";
  text: string;
}

export interface ThoughtContent {
  type: "thought";
  text: string;
}

export interface DiffContent {
  type: "diff";
  diff: {
    path: string;
    old: string;
    new: string;
    /** Optional unified-diff style hunks when available. */
    hunks?: Array<{ old: { start: number; lines: string[] }; new: { start: number; lines: string[] } }>;
  };
}

export interface CommandOutputContent {
  type: "command_output";
  /** The shell command that was (or is being) run. */
  command?: string;
  /** Stdout+stderr captured so far. */
  output: string;
  exitCode?: number | null;
}

/** An image produced by a tool (read_file on an image/PDF in EchoAgent).
 *  Mirrors ACP `ContentBlock::Image` after unwrapping the outer
 *  `{ type: "content", content: … }` envelope (see normalizeToolCallContent). */
export interface ImageToolContent {
  type: "image";
  /** Base64-encoded image bytes. */
  data: string;
  mimeType: string;
  /** Optional source URI (http/file) when the tool references a file. */
  uri?: string;
}

export type ToolCallContent =
  | TextContent
  | DiffContent
  | CommandOutputContent
  | ImageToolContent;

// ---------- tool call status ----------

export type ToolCallStatus = "pending" | "in_progress" | "completed" | "failed";

// Known EchoAgent tool kinds (from echo-agent-tools). The wire format allows unknown
// kinds too — render them generically.
export type ToolKind =
  | "read_file"
  | "edit"
  | "grep"
  | "list_dir"
  | "run_terminal_command"
  | "web_search"
  | "web_fetch"
  | "todo_write"
  | "spawn_subagent"
  | "memory_search"
  | string; // forward-compat

// ---------- session updates (the agent -> client stream) ----------

/** ACP content is one block per chunk; rendered tool content is normalized separately. */
export type AcpContentBlock = (TextContent | ImageToolContent
  | { type: "audio"; data: string; mimeType: string }
  | { type: "resource_link"; uri: string; name: string; mimeType?: string }
  | { type: "resource"; resource: { uri: string; mimeType?: string; text?: string; blob?: string } }
) & { _meta?: Record<string, unknown> };

export interface AgentMessageChunk {
  sessionUpdate: "agent_message_chunk";
  content: AcpContentBlock;
}

export interface AgentThoughtChunk {
  sessionUpdate: "agent_thought_chunk";
  content: AcpContentBlock;
}

export type AcpToolCallContent =
  | { type: "content"; content: AcpContentBlock }
  | { type: "diff"; path: string; oldText?: string | null; newText: string }
  | { type: "terminal"; terminalId: string };

export interface ToolCallUpdate {
  sessionUpdate: "tool_call";
  toolCallId: string;
  title: string;
  /** ACP omits the default `other` kind and `pending` status. */
  kind?: ToolKind;
  status?: ToolCallStatus;
  /** Raw input the tool was invoked with, when the agent sends it inline. */
  rawInput?: unknown;
  rawOutput?: unknown;
  content?: AcpToolCallContent[];
  locations?: Array<{ path: string; line?: number | null }>;
}

export interface ToolCallDeltaUpdate extends Partial<Omit<ToolCallUpdate, "sessionUpdate" | "toolCallId">> {
  sessionUpdate: "tool_call_update";
  toolCallId: string;
}

/** ACP flattens Plan.entries; retain the older nested-plan transport too. */
export type PlanUpdate = (
  | { sessionUpdate: "plan"; type?: "plan" }
  | { type: "plan"; sessionUpdate?: "plan" }
) & (
  | { entries: PlanEntry[]; plan?: Plan | null }
  | { plan: Plan | null; entries?: PlanEntry[] }
);

/** A EchoAgent execution plan (ACP `Plan`). Each update replaces the whole plan. */
export interface Plan {
  entries: PlanEntry[];
}

export interface PlanEntry {
  /** Human-readable description of this task. */
  content: string;
  /** "high" | "medium" | "low". */
  priority: PlanEntryPriority;
  /** "pending" | "in_progress" | "completed". */
  status: PlanEntryStatus;
}

export type PlanEntryPriority = "high" | "medium" | "low";
export type PlanEntryStatus = "pending" | "in_progress" | "completed";

export interface UsageUpdate {
  sessionUpdate?: "usage_update";
  type?: "usage_update";
  usage: {
    promptTokens?: number;
    completionTokens?: number;
    totalTokens?: number;
  };
}

/** Historical `type` transport retained explicitly for stored/legacy inputs. */
export interface ExtensionSessionUpdate {
  type: string;
  [key: string]: unknown;
}

export interface SessionUpdateMetadata {
  sessionId?: string;
  __sessionId?: string;
  _meta?: Record<string, unknown>;
}

/** The current ACP subset consumed by the desktop. No catch-all tag may hide malformed known messages. */
export type AcpSessionUpdate = (
  | AgentMessageChunk
  | AgentThoughtChunk
  | { sessionUpdate: "user_message_chunk"; content: AcpContentBlock }
  | ToolCallUpdate
  | ToolCallDeltaUpdate
  | { sessionUpdate: "plan"; entries: PlanEntry[] }
  | { sessionUpdate: "current_mode_update"; currentModeId: string }
  | { sessionUpdate: "available_commands_update"; availableCommands: unknown[] }
) & SessionUpdateMetadata;

/** Host-owned extensions are distinct from the closed ACP wire subset. */
export type HostSessionUpdate = (
  | { sessionUpdate: "plan_approval_request"; requestId: string; toolCallId: string; planContent?: string }
  | { sessionUpdate: "plan_approval_resolved"; requestId: string }
  | ({ sessionUpdate: "turn_completed" | "session_summary_generated" | "subagent_spawned" | "subagent_progress" | "subagent_finished" } & Record<string, unknown>)
) & SessionUpdateMetadata;

// ---------- context usage (echo.agent/session/info + echo.agent/session/usage) ----------

/** One itemized context-cost row from EchoAgent (skills listing, MCP servers). */
export interface TokenUsageCategory {
  /** Display label, e.g. "Skills" or "MCP servers". */
  label: string;
  tokens: number;
  /** Supporting detail, e.g. "21 skills". */
  detail?: string;
}

/**
 * Context-window snapshot from EchoAgent's `echo.agent/session/info`
 * (`ContextInfo` in echo-agent-runtime, camelCase on the wire).
 * Note: skills/MCP category estimates overlap `messageTokens` (they're
 * injected as system-reminders in messages), so category percentages are
 * approximate — the UI clamps the "其他" remainder at 0.
 */
export interface ContextInfo {
  used: number;
  /** Context window size of the active model. */
  total: number;
  usagePct: number;
  systemPromptTokens: number;
  toolDefinitionsCount: number;
  toolDefinitionsTokens: number;
  messageCount: number;
  messageTokens: number;
  turnCount: number;
  toolCallCount: number;
  compactionCount: number;
  freeTokens: number;
  autoCompactThresholdPercent?: number;
  usageCategories?: TokenUsageCategory[];
}

/** Wire response of `echo.agent/session/info` (only the fields the UI consumes). */
export interface SessionInfoResponse {
  sessionId: string;
  cwd: string;
  model?: string | null;
  modelDisplayName?: string;
  context: ContextInfo;
}

/**
 * Cumulative session token usage from `echo.agent/session/usage` (`PromptUsage`
 * totals, camelCase on the wire). `inputTokens` includes cache reads, so the
 * average cache hit rate is `cachedReadTokens / inputTokens`.
 */
export interface SessionUsage {
  inputTokens: number;
  outputTokens: number;
  totalTokens: number;
  cachedReadTokens: number;
  cacheCreationTokens?: number;
  reasoningTokens?: number;
  modelCalls?: number;
  apiDurationMs?: number;
  costUsdTicks?: number;
  costIsPartial?: boolean;
  modelUsage?: Record<string, SessionUsageModel>;
  numTurns?: number;
  usageIsIncomplete?: boolean;
}

/** Per-model row nested under PromptUsage.modelUsage. */
export interface SessionUsageModel {
  inputTokens: number;
  outputTokens: number;
  totalTokens: number;
  cachedReadTokens: number;
  cacheCreationTokens?: number;
  reasoningTokens?: number;
  modelCalls?: number;
  apiDurationMs?: number;
  costUsdTicks?: number;
  costIsPartial?: boolean;
}

/** Exact per-prompt usage forwarded from the durable TurnCompleted update. */
export interface TurnUsageEvent {
  sessionId: string;
  promptId: string;
  usage: SessionUsage;
  occurredAt?: number;
  eventId?: string;
}

export type SessionUpdate = (
  | AcpSessionUpdate
  | HostSessionUpdate
  | PlanUpdate
  | UsageUpdate
  | ExtensionSessionUpdate
) & SessionUpdateMetadata;

// ---------- permissions ----------

/**
 * ACP currently defines one-shot and persistent outcomes for both approval and
 * rejection. `other` keeps the frontend honest when a newer runtime sends an
 * option it does not understand yet: the choice is still shown to the user.
 */
export type PermissionKind =
  | "allow"
  | "allow_always"
  | "deny"
  | "deny_always"
  | "other";

export interface PermissionOption {
  optionId: string;
  kind: PermissionKind;
  title: string;
}

export interface PermissionRequest {
  /** Echoed back in `agent_resolve_permission`. */
  requestId: string;
  sessionId: string;
  toolCallId: string;
  toolKind: ToolKind;
  title: string;
  /** Optional partial raw input to show the user what they're approving. */
  rawInput?: unknown;
  options: PermissionOption[];
}

// ---------- prompt completion ----------

export type StopReason =
  | "end_turn"
  | "max_turns"
  | "rate_limited"
  | "cancelled"
  | string;

export interface PromptComplete {
  sessionId: string;
  promptId: string;
  turnId?: number;
  stopReason: StopReason;
  cancelTrigger?: string;
  /** Structured reason supplied by EchoAgent for cancelled/stationary turns. */
  cancellationCategory?: string;
  /** Provider/runtime detail for terminal failures, when safe to surface. */
  agentResult?: string;
  usage?: {
    promptTokens?: number;
    completionTokens?: number;
    totalTokens?: number;
  };
}

// ---------- session metadata ----------

/** Lifecycle status for sidebar filtering and user-action visibility.
 *  - "working": actively streaming a response
 *  - "completed": finished normally
 *  - "failed": errored during send/stream
 *  - "pending": created but no message sent yet
 *  - "planning": actively producing or revising a plan
 *  - "awaiting_*": paused for a specific user interaction
 *  - "pausing"/"stopping": a session-scoped control request is being sent
 *  - "paused": user paused the turn and explicitly retained its context
 *  - "stopped": cancelled by the user or by a non-error external trigger */
export type SessionStatus =
  | "working"
  | "completed"
  | "failed"
  | "pending"
  | "planning"
  | "awaiting_permission"
  | "awaiting_answer"
  | "awaiting_approval"
  | "pausing"
  | "paused"
  | "stopping"
  | "stopped";

export interface SessionSummary {
  sessionId: string;
  /** Human-readable title. Display priority matches EchoAgent's `display_title`:
   * `generated_title` (LLM-generated or manual /rename) > `session_summary`
   * (user's first prompt text). */
  title: string;
  /** ISO timestamp of last activity (`updated_at`, falling back to `last_active_at`). */
  updatedAt?: string;
  /** Working directory the session is bound to. */
  cwd: string;
  /** True if it's a git repo (inferred from `git_root_dir` in summary.json). */
  isGitRepo?: boolean;
  /** True if the session is pinned to the top of the list.
   *  EchoAgent-only state (EchoAgent has no pinned field); stored in
   *  `~/.echo-agent/echoagent-state.json`. */
  pinned?: boolean;
  /** True if the session is archived (hidden from the sidebar).
   *  EchoAgent-only state (EchoAgent has no archived field); stored in
   *  `~/.echo-agent/echoagent-state.json`. */
  archived?: boolean;
  /** Upstream classification such as `subagent`; used for hierarchy-aware UI. */
  sessionKind?: string;
  /** Hidden child/internal sessions are addressable but not catalog tasks. */
  hidden?: boolean;
  /** Model id bound to this session, if recorded in summary.json. */
  currentModelId?: string;
  /** Permission mode owned and persisted by this task/session. */
  permissionMode?: "ask" | "auto" | "always-approve";
  /** Expert id bound to this session (EchoAgent-only state). */
  expertId?: string;
  /** Expert display name (EchoAgent-only state). */
  expertName?: string;
  /** Expert local avatar path (EchoAgent-only state). */
  expertAvatar?: string;
  /** Lifecycle status for task filtering. Absent means a legacy record whose
   *  terminal state was not captured; consumers must not present it as proven
   *  task completion. */
  status?: SessionStatus;
}

/** Payload of the `agent://summary` event — a freshly generated or renamed
 *  session title pushed by EchoAgent via `echo.agent/session_notification`
 *  (`SessionSummaryGenerated` variant). */
export interface SessionSummaryEvent {
  sessionId: string;
  title: string;
}

// ---------- skills (echo.agent/skills/*) ----------

/** One discovered skill. Mirrors EchoAgent's `SkillInfo`. */
export interface SkillInfo {
  name: string;
  displayName?: string;
  description?: string;
  /** Where the skill was discovered: "local" | "repo" | "user" | "server" | "bundled" | "plugin". */
  scope?: string;
  enabled: boolean;
  userInvocable?: boolean;
  /** Filesystem path to the skill directory (when available). */
  path?: string;
  /** Package is copied into and owned by EchoAgent's managed local installer. */
  managed?: boolean;
  /** Signed package synchronized from the organization server. */
  orgManaged?: boolean;
  orgSkillId?: string;
  orgVersionId?: string;
  orgScopeKind?: "personal" | "team" | "org";
  orgMandatory?: boolean;
  orgAllowPersonalOverride?: boolean;
  version?: string;
  author?: string;
  license?: string;
  compatibility?: string;
  whenToUse?: string;
  /** Exact custom `[skills].paths` registration that can be removed safely. */
  configuredPath?: string;
  /** Runtime/dependency/account readiness from an optional echo.skill.json contract. */
  capability?: SkillCapabilityReport;
}

export type SkillRiskLevel = "low" | "medium" | "high";

export interface SkillRiskFinding {
  level: SkillRiskLevel;
  code: string;
  message: string;
  path?: string;
}

export type SkillCapabilityState =
  | "instruction_only"
  | "ready"
  | "missing_dependencies"
  | "configuration_required"
  | "invalid";

export type SkillCapabilityCheckStatus =
  | "ready"
  | "missing"
  | "configuration_required"
  | "declared"
  | "invalid";

export interface SkillCapabilityCheck {
  kind: "manifest" | "command" | "entrypoint" | "connector" | "os_permission" | string;
  key: string;
  status: SkillCapabilityCheckStatus;
  message: string;
}

export interface SkillConnectorRequirement {
  id: string;
  label?: string;
  accountRequired: boolean;
  purpose?: string;
}

export interface SkillCapabilityManifest {
  schemaVersion: number;
  capabilities: string[];
  runtime?: {
    kind: "python" | "node" | "shell";
    command?: string;
    entrypoints: Record<string, string>;
    timeoutSeconds: number;
  };
  requirements: {
    commands: string[];
    connectors: SkillConnectorRequirement[];
    osPermissions: string[];
  };
  permissions: {
    filesystem: "none" | "workspace-read" | "workspace-write";
    network: string[];
    externalActions: string[];
  };
  artifacts: Array<{
    id: string;
    pattern: string;
    mimeType?: string;
    required: boolean;
    maxBytes?: number;
  }>;
}

export interface SkillCapabilityReport {
  declared: boolean;
  state: SkillCapabilityState;
  ready: boolean;
  capabilities: string[];
  checks: SkillCapabilityCheck[];
  manifest?: SkillCapabilityManifest;
}

export interface SkillPackageInspection {
  sourcePath: string;
  /** Relative Skill root when a directory or ZIP contains multiple Skills. */
  packageRoot?: string;
  name: string;
  description: string;
  version?: string;
  fileCount: number;
  totalBytes: number;
  riskLevel: SkillRiskLevel;
  findings: SkillRiskFinding[];
  warnings: string[];
  /** Absent only when talking to an older backend. */
  capability?: SkillCapabilityReport;
  sourceHash: string;
  alreadyInstalled: boolean;
  installedPath?: string;
}

/** Per-Skill result returned while expanding a folder or ZIP bundle. */
export interface SkillPackageInspectionOutcome {
  label: string;
  packageRoot?: string;
  inspection?: SkillPackageInspection;
  error?: string;
}

export interface SkillInstallResult {
  installedPath: string;
  updated: boolean;
  inspection: SkillPackageInspection;
}

// ---------- connectors / MCP (echo.agent/mcp/*) ----------

/** One MCP server config entry surfaced to the UI. */
export interface McpServerEntry {
  name: string;
  displayName?: string;
  /** "stdio" | "streamable_http". */
  transport?: string;
  /** For stdio: command. For http: URL. */
  target?: string;
  enabled: boolean;
  /** "user" | "project" | "bundled" | ... */
  source?: string;
  disabledReason?: string;
  vendor?: string;
  /** Live health reported by the current session. */
  status?: "ready" | "initializing" | "setuprequired" | "unavailable" | string;
  live: boolean;
  authRequired: boolean;
  setupRequired: boolean;
  setup?: McpSetupConfig;
  setupValues: Record<string, string>;
  tools: McpToolEntry[];
  args: string[];
  env: Record<string, string>;
  editable: boolean;
}

export interface McpSetupConfig {
  fields: McpSetupField[];
  variables: Record<string, unknown>;
}

export interface McpSetupField {
  id: string;
  label: string;
  type: "select";
  required: boolean;
  default?: string;
  options: Array<{ label: string; value: string }>;
}

export interface McpToolEntry {
  name: string;
  displayName?: string;
  description?: string;
  enabled: boolean;
}

/** Frontend payload for creating/updating an MCP server. */
export interface McpUpsertRequest {
  name: string;
  /** "stdio" or "http". */
  transport: string;
  /** stdio: command. http: URL. */
  target: string;
  args?: string[];
  env?: Record<string, string>;
  headers?: Record<string, string>;
  enabled?: boolean;
  cwd?: string;
  bearerTokenEnvVar?: string;
  oauthClientId?: string;
  oauthClientSecretEnvVar?: string;
  oauthScopes?: string[];
  startupTimeoutSec?: number;
  toolTimeoutSec?: number;
  oauth?: {
    clientId?: string;
    clientSecretEnvVar?: string;
    scopes?: string[];
    callbackPort?: number;
  };
  setup?: {
    fields?: Array<Record<string, unknown>>;
    variables?: Record<string, unknown>;
  };
  toolTimeouts?: Record<string, number>;
  exposeImageBase64?: boolean;
}

export interface McpMutationResult {
  persisted: boolean;
  appliedLive: boolean;
  warnings: string[];
}

export interface McpConfigSaveResult {
  serverCount: number;
  removedCount: number;
  appliedLive: boolean;
  warnings: string[];
}

/** Result of `mcp_auth_trigger` (browser OAuth flow driven by EchoAgent). */
export interface McpAuthTriggerResult {
  /** "authenticated" | "failed" | "setup_required". */
  status: string;
  error?: string;
}

/** One entry of `mcp_auth_status` — a server EchoAgent flagged as needing auth. */
export interface McpAuthStatusEntry {
  serverName: string;
  status: string;
}

// ---------- experts / assistants (~/.echo-agent/agents/*.md) ----------

/** One agent definition (subagent template). */
export interface AgentEntry {
  name: string;
  description?: string;
  /** "user" | "project". */
  scope: string;
  /** Absolute path to the `.md` file. */
  path: string;
  /** Full file contents (frontmatter + body), for the editor view. */
  raw: string;
  /** Avatar preset index 1-20 (EchoAgent-style). Undefined = name-initial fallback. */
  avatar?: number;
  /** Model capability tags: subset of ["default", "multimodal", "reasoning"]. */
  modelTags?: string[];
}

// ---------- permission rules (~/.echo-agent/config.toml [permission]) ----------

/** One permission rule. `action` ∈ allow|deny|ask; `tool` ∈ bash|read|edit|grep|mcp|any. */
export interface PermissionRule {
  action: string;
  tool: string;
  pattern?: string;
}

// ---------- memory (资料库 — ~/.echo-agent/memory/) ----------

export interface MemoryEntry {
  scope: "global" | "workspace" | "session";
  path: string;
  content: string;
  size: number;
  revision: string;
  modifiedAt: string | null;
  readOnly: boolean;
}

// ---------- session search ----------

export interface SearchHit {
  sessionId: string;
  cwd?: string;
  title?: string;
  snippet?: string;
  rank?: number;
  updatedAt?: string;
}

// ---------- rewind ----------

/** Canonical Runtime wire values for selecting what a rewind restores. */
export type RewindMode = "all" | "conversation_only" | "files_only";

export interface RewindPoint {
  promptIndex: number;
  promptPreview?: string;
  timestamp?: string;
  /** First assistant response snippet (for timeline display). */
  messagePreview?: string;
  /** Whether this prompt produced file changes. */
  hasFileChanges?: boolean;
  /** Whether this prompt produced memory writes. */
  hasMemoryChanges?: boolean;
  /** Tool calls made during this turn. */
  toolNames?: string[];
}

/** Result of a committed conversation/file rewind. For message regeneration,
 * `promptText` preserves the exact model-facing prompt that was removed. */
export interface RewindExecution {
  targetPromptIndex: number;
  promptText?: string;
}

// ---------- slash commands + prompt history ----------

export interface SlashCommand {
  name: string;
  description?: string;
  argumentHint?: string;
  source?: string;
}

// ---------- tasks / subagents ----------

export interface RunningTask {
  id: string;
  /** Native endpoint that owns this id and must handle cancellation. */
  source: "task" | "subagent";
  kind?: string;
  description?: string;
  status?: string;
  sessionId?: string;
}

// ---------- subagent live events (agent://subagent) ----------

/** A live or replayed subagent lifecycle event forwarded from EchoAgent. */
export interface SubagentLiveEvent {
  /** Parent session that owns the subagent. */
  sessionId: string;
  /** Lifecycle phase: "spawned" | "progress" | "finished". */
  phase: "spawned" | "progress" | "finished";
  /** Subagent unique id (= child session id). */
  subagentId: string;
  childSessionId?: string;
  /** Stable id of the parent turn that created the child. */
  parentPromptId?: string;
  description?: string;
  subagentType?: string;
  model?: string;
  persona?: string;
  role?: string;
  effectiveContextSource?: string;
  contextNormalized?: boolean;
  capabilityMode?: string;
  resumedFrom?: string;
  workflowRunId?: string;
  /** "running" (spawned/progress) or the finished status. */
  status?: string;
  durationMs?: number;
  turnCount?: number;
  toolCallCount?: number;
  tokensUsed?: number;
  contextWindowTokens?: number;
  contextUsagePct?: number;
  toolsUsed?: string[];
  errorCount?: number;
  error?: string;
  output?: string;
  occurredAt?: number;
  isReplay?: boolean;
}

/**
 * A turn that ended abnormally. EchoAgent reports mid-stream failures (e.g. a 429
 * rate limit hit while a tool was executing) via `prompt_complete` with
 * `stopReason: "rate_limit" | "error"` rather than as a thrown error. The
 * backend forwards these as `agent://turn-error` so the UI can surface a
 * friendly explanation instead of silently marking the turn complete.
 */
export interface TurnErrorEvent {
  sessionId: string;
  /** Exact failing turn, shared with the terminal completion event. */
  promptId?: string;
  /** "rate_limit" | "error" (mirrors EchoAgent's `stop_reason_for_turn_error`). */
  kind: "rate_limit" | "error";
  /** Server-provided detail (absent for rate_limit — EchoAgent omits it so the
   *  client shows its own message). */
  detail?: string;
}

// ---------- automations (local scheduler, EchoAgent 1:1) ----------

export type ScheduleFreq = "DAILY" | "WEEKLY" | "MONTHLY" | "YEARLY" | "HOURLY";

/** RRULE-like recurring schedule. 双周 = WEEKLY interval 2; 按间隔 = HOURLY + intervalHours. */
export interface AutomationSchedule {
  freq: ScheduleFreq;
  interval: number;
  /** Weekday codes "MO".."SU". */
  byday: string[];
  /** Days of month 1..=31 (MONTHLY/YEARLY). */
  bymonthday: number[];
  /** Months 1..=12 (YEARLY). */
  bymonth: number[];
  byhour: number;
  byminute: number;
  intervalHours: number;
}

export type AutomationScheduleType = "recurring" | "once";
export type AutomationPermissionMode = "fullAccess" | "default";
export type AutomationStatus = "ACTIVE" | "PAUSED";

// ---------- agent / assistant defaults (~/.echo-agent/config.toml) ----------

export interface AgentDefaults {
  /** Model id for new sessions (`[models] default`). Empty = EchoAgent's built-in. */
  defaultModel: string;
  /** Default permission selection (`[ui] default_selected_permission`). */
  defaultPermission: string;
  /** Show "Always allow" options on prompts (`[ui] remember_tool_approvals`). */
  rememberToolApprovals?: boolean;
}

// ---------- local plugins (echo.agent/plugins/*) ----------

/** One installed plugin (subset of EchoAgent's PluginInfo). */
export interface PluginEntry {
  origin?: { type: string; sourceName?: string; gitUrl?: string };
  management?: { uninstallable: boolean; removable: boolean; packagePlugins: string[]; removalPath?: string | null; pathPlugins?: string[] };
  name: string;
  id?: string;
  root?: string;
  scope?: string;
  trusted?: boolean;
  enabled: boolean;
  version?: string;
  description?: string;
  skillCount?: number;
  skillNames?: string[];
  agentCount?: number;
  agentNames?: string[];
  hookStatus?: string;
  hookCount?: number;
  mcpServerCount?: number;
  mcpStatus?: string;
  conflict?: unknown;
}

export interface PluginsListResponse {
  plugins: PluginEntry[];
}

// ---------- notification center ----------

export type NotificationKind =
  | "permission"
  | "folder_trust"
  | "task_update"
  | "plan_mode"
  | "mcp_status"
  | "models_update"
  | "summary"
  | "session_complete"
  | "error"
  | "info";

export interface NotificationEntry {
  id: number;
  kind: NotificationKind | string;
  at: string;
  title: string;
  body?: string;
  sessionId?: string;
  requestId?: string;
  automationId?: string;
  severity: "info" | "warn" | "error" | string;
  read: boolean;
}

export interface Automation {
  id: string;
  name: string;
  prompt: string;
  /** Comma-separated workspace directories (first entry is the run cwd). */
  cwds: string;
  status: AutomationStatus;
  modelId?: string;
  modelIsThinking?: boolean;
  skills: string[];
  expertId?: string;
  expertName?: string;
  /** Absolute stable definition path captured when selecting the expert. */
  expertPath?: string;
  connectorIds: string[];
  permissionMode: AutomationPermissionMode;
  scheduleType: AutomationScheduleType;
  schedule: AutomationSchedule;
  /** Once mode: YYYY-MM-DD. */
  scheduledDate?: string;
  /** Once mode: HH:MM. */
  scheduledTime?: string;
  /** Recurring validity window (YYYY-MM-DD, inclusive). */
  validFromDate?: string;
  validUntilDate?: string;
  pushToWeChat: boolean;
  /** Legacy tasks default to independent conversations. */
  newSessionEachRun?: boolean;
  contextGeneration?: number;
  dedicatedSessionId?: string;
  lastRunAt?: string;
  nextRunAt?: string;
  createdAt: string;
}

/** A single run-history entry (运行记录). */
export interface AutomationRunRecord {
  promptId?: string;
  stopRequested?: boolean;
  needsAttention?: boolean;
  waitingReason?: string;
  executionConfig?: { prompt: string; newSessionEachRun: boolean; permissionMode: AutomationPermissionMode };
  completionPending?: boolean;
  notificationDelivery?: string;
  id: string;
  automationId: string;
  automationName: string;
  status: "queued" | "running" | "success" | "failed" | "interrupted" | "stopped" | string;
  startedAt: string;
  finishedAt?: string;
  sessionId?: string;
  cwd?: string;
  modelId?: string;
  scheduledFor?: string;
  error?: string;
  archived: boolean;
}

export interface AutomationSnapshot {
  automations: Automation[];
  records: AutomationRunRecord[];
}

/** Live lifecycle notification emitted by the native automation scheduler. */
export interface AutomationUpdateEvent {
  phase: "queued" | "running" | "sessionCreated" | "finished" | string;
  automationId: string;
  automationName: string;
  recordId: string;
  status: AutomationRunRecord["status"];
  sessionId?: string;
  promptId?: string;
  currentModelId?: string;
  cwd?: string;
  error?: string;
}

/** Raw mcp.json file content returned by the `mcp_config_read` command. */
export interface McpConfigFile {
  filePath: string;
  content: string;
}
