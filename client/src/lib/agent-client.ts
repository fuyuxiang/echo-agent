/**
 * agent-client — typed wrappers over the EchoAgent Tauri commands and events.
 *
 * The Rust backend (src-tauri/src/commands.rs) exposes a command table that
 * drives the in-process EchoAgent agent over ACP. Streamed updates arrive as the
 * `agent://update`, `agent://permission`, `agent://complete` events, whose
 * payloads are the types in ./types.ts.
 */
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type {
  AgentDefaults,
  AgentEntry,
  Automation,
  AutomationUpdateEvent,
  ExpertCatalog,
  AutomationSnapshot,
  AutomationStatus,
  ConnectorCatalog,
  ConnectorCliAuthDoneEvent,
  ConnectorCliAuthLogEvent,
  ConnectorCliAuthResult,
  ConnectorCliAuthUrlEvent,
  ConnectorCliStatus,
  McpAuthStatusEntry,
  McpAuthTriggerResult,
  McpConfigFile,
  McpConfigSaveResult,
  McpMutationResult,
  McpServerEntry,
  McpUpsertRequest,
  MemoryEntry,
  PermissionRequest,
  PermissionRule,
  PromptComplete,
  RewindExecution,
  RewindMode,
  RewindPoint,
  RunningTask,
  SearchHit,
  SessionInfoResponse,
  SessionSummary,
  SessionSummaryEvent,
  SessionUpdate,
  SessionUsage,
  SkillCatalog,
  SkillInfo,
  SkillInstallResult,
  SkillPackageInspection,
  SkillPackageInspectionOutcome,
  SlashCommand,
  SubagentLiveEvent,
  TurnErrorEvent,
  TurnUsageEvent,
} from "./types";

import type { QuestionRequest } from "@/stores/question-store";
import type { PlanApprovalRequest } from "@/stores/session-store";
import { isUpstreamBrandedModelId } from "@/lib/model-branding";
import { preparePromptWithPersonalKnowledge } from "@/lib/knowledge-context";
import {
  organizationScopeIdsForSession,
  useKnowledgeStore,
  type KnowledgeSource,
} from "@/stores/knowledge-store";
import {
  organizationKnowledgeAvailability,
  useOrgSessionStore,
} from "@/stores/org-session-store";

// ---------- commands ----------

export interface AuthStatus {
  ready: boolean;
  /** Human-readable reason when not ready. */
  reason?: string;
  /** Model ids configured in ~/.echo-agent/config.toml (BYOK providers). */
  providers: string[];
  /** Whether Runtime acknowledged the current model configuration. */
  runtimeReady: boolean;
  /** Whether Runtime installed auth for the acknowledged model configuration. */
  runtimeAuthReady?: boolean;
  /** Whether Runtime and disk model configuration revisions match. */
  synchronized: boolean;
  /** Model ids in the last Runtime-acknowledged configuration. */
  runtimeModels: string[];
  /** Concrete configured model used by automatic model selection. */
  defaultModelId?: string;
  lastRuntimeError?: string;
}

export interface InitResult {
  /** Whether the agent initialized successfully. */
  ok: boolean;
  auth: AuthStatus;
  /** The cwd the agent bound to (echoes the input). */
  cwd: string;
  agentVersion?: string;
  /** Default model id the agent will use. */
  defaultModelId?: string;
  /** Build identity from the native binary, independent of frontend assets. */
  buildCommit: string;
  buildTime: string;
  buildCommitTime: string;
  logDir: string;
}

/**
 * Initialize the in-process EchoAgent agent. If `cwd` is omitted the backend
 * uses EchoAgent's dedicated default working directory.
 */
export async function agentInit(cwd?: string): Promise<InitResult> {
  invalidateAgentKnowledgeSourceSync();
  return invoke<InitResult>("agent_init", { cwd: cwd ?? null });
}

export async function agentAuthStatus(): Promise<AuthStatus> {
  return invoke<AuthStatus>("agent_auth_status");
}

export interface DesktopPreferences {
  /** Keep the native process and Runtime alive when the main window closes. */
  closeToTray: boolean;
  showNotificationTaskTitle: boolean;
}

export async function desktopPreferencesGet(): Promise<DesktopPreferences> {
  return invoke<DesktopPreferences>("desktop_preferences_get");
}

export async function desktopPreferencesSave(closeToTray: boolean): Promise<DesktopPreferences> {
  return invoke<DesktopPreferences>("desktop_preferences_save", { closeToTray });
}

export async function desktopNotificationPreviewSave(showTaskTitle: boolean): Promise<DesktopPreferences> {
  return invoke<DesktopPreferences>("desktop_notification_preview_save", { showTaskTitle });
}

// NOTE: the backend `agent_new_session` command returns the session id as a
// bare `String` (see commands.rs agent_new_session). We type it as `string`
// here — do NOT wrap it in `{ sessionId }`, or callers destructuring
// `const { sessionId } = ...` will silently get undefined.
//
// `modelId` is passed as `_meta.modelId` to EchoAgent so the session binds to
// that model from the start (avoids the default `echo-agent-build` model whose
// sampling config has no key in a BYOK-only setup).
export async function agentNewSession(
  cwd: string,
  modelId?: string,
  permissionMode: PermissionMode = "ask",
): Promise<string> {
  const invocation = invoke<string>("agent_new_session", {
    cwd,
    modelId: modelId ?? null,
    permissionMode,
  });
  let timeoutId: ReturnType<typeof setTimeout> | undefined;
  const timeout = new Promise<never>((_, reject) => {
    timeoutId = setTimeout(() => {
      // Last-resort guard only: the backend now bounds this itself at 45s and
      // reclaims a session that the Runtime finishes after the timeout, so a
      // retry adopts that session instead of creating a duplicate.
      reject(new Error("创建 Agent 会话超时（50 秒），请重试；若持续失败请完全退出后重启应用。"));
    }, 50_000);
  });
  try {
    return await Promise.race([invocation, timeout]);
  } finally {
    if (timeoutId !== undefined) clearTimeout(timeoutId);
  }
}

// Loading replays history through agent://update and returns the runtime's
// actual session model. Never substitute the global default for this value.
export async function agentLoadSession(sessionId: string, cwd: string): Promise<string | null> {
  // A load can recreate the resident Runtime session, so any renderer-side MCP
  // acknowledgement for the previous resident instance is stale.
  invalidateAgentKnowledgeSourceSync(sessionId);
  return invoke<string | null>("agent_load_session", { sessionId, cwd });
}

export async function agentListSessions(
  cwd: string,
  includeArchived = false,
): Promise<SessionSummary[]> {
  return invoke<SessionSummary[]>("agent_list_sessions", { cwd, includeArchived });
}

/** List persisted sessions across every historical working directory. */
export async function agentListAllSessions(
  includeArchived = false,
): Promise<SessionSummary[]> {
  return invoke<SessionSummary[]>("agent_list_all_sessions", { includeArchived });
}

/** A discovered working directory (EchoAgent has run sessions in it). */
export interface WorkspaceInfo {
  /** Absolute path of the working directory. */
  cwd: string;
  /** Number of sessions recorded under this cwd. */
  sessionCount: number;
  /** Title of the most recent session under this cwd (optional, for display). */
  lastTitle?: string;
}

/**
 * List every working directory EchoAgent has ever seen (deduplicated), with a
 * session count per cwd. Used to populate the Composer's working-directory picker.
 */
export async function agentListWorkspaces(): Promise<WorkspaceInfo[]> {
  return invoke<WorkspaceInfo[]>("agent_list_workspaces");
}

/** Pick files in a native dialog and grant only those exact canonical files. */
export async function filesystemPickFiles(options?: {
  title?: string;
  extensions?: string[];
  multiple?: boolean;
  maxFiles?: number;
}): Promise<string[]> {
  return invoke<string[]>("filesystem_pick_files", {
    title: options?.title ?? null,
    extensions: options?.extensions ?? null,
    multiple: options?.multiple ?? null,
    maxFiles: options?.maxFiles ?? null,
  });
}

export interface AttachmentFileStat {
  inputPath: string;
  /** Canonical path returned by the native capability boundary. */
  path: string;
  sizeBytes: number;
}

export interface AttachmentStatsResult {
  files: AttachmentFileStat[];
  rejected: Array<{ path: string; reason: string }>;
}

/**
 * Validate picker/drop paths and return their real on-disk sizes before the
 * Composer reports that they were added. Paths must already have been granted
 * by a native picker, native drag event, workspace, or the managed paste store.
 */
export async function filesystemAttachmentStats(
  paths: string[],
): Promise<AttachmentStatsResult> {
  return invoke<AttachmentStatsResult>("filesystem_attachment_stats", { paths });
}

/**
 * Switch the model used by an existing session (EchoAgent's `session/set_model`).
 * May reject with `MODEL_SWITCH_INCOMPATIBLE_AGENT` if the session has turns
 * and the new model requires a different agent harness — surface that error
 * to the user (suggest starting a new session).
 */
export async function agentSetModel(sessionId: string, modelId: string): Promise<void> {
  await invoke<void>("agent_set_model", { sessionId, modelId });
}

export interface AgentKnowledgeSourcesResult {
  personalSelected: boolean;
  organizationSelected: boolean;
  personalAttached: boolean;
  organizationAttached: boolean;
}

export const KNOWLEDGE_MCP_SERVER_NAME = "echoagent_organization_memory";

export interface McpServerStatusEvent {
  sessionId: string;
  name: string;
  source: "local" | "managed" | string;
  status: "ready" | "initializing" | "unavailable" | "needsauth" | string;
  reason:
    | "transport_closed"
    | "handshake_failed"
    | "config_added"
    | "config_removed"
    | "config_changed"
    | "disabled"
    | "auth_expired"
    | "initialized"
    | "restart_succeeded"
    | "restart_failed"
    | "managed_token_refreshed"
    | string;
  detail?: string;
  tools?: unknown;
}

interface AppliedKnowledgeSources {
  key: string;
  result: AgentKnowledgeSourcesResult;
}

const appliedKnowledgeSources = new Map<string, AppliedKnowledgeSources>();

function knowledgeSourcesKey(sources: KnowledgeSource[]): string {
  return `${sources.includes("personal") ? "1" : "0"}:${sources.includes("organization") ? "1" : "0"}`;
}

function knowledgeConfigurationKey(sources: KnowledgeSource[], organizationScopeIds: string[]): string {
  const organizationIdentity = sources.includes("organization")
    ? organizationKnowledgeAvailability().identity
    : "local-only";
  return `${knowledgeSourcesKey(sources)}:${organizationIdentity}:${[...organizationScopeIds].sort().join(",")}`;
}

function organizationSelectionError(organizationScopeIds: string[]): string | undefined {
  const availability = organizationKnowledgeAvailability();
  if (!availability.available) return availability.reason;

  const allowedScopeIds = new Set(
    useOrgSessionStore.getState().session?.bootstrap?.scopes.map((scope) => scope.id) ?? [],
  );
  if (organizationScopeIds.some((scopeId) => !allowedScopeIds.has(scopeId))) {
    return "原组织知识范围已失效，请重新选择知识范围";
  }
  return undefined;
}

/** Invalidate MCP reconciliation acknowledgements after a Runtime/session lifecycle boundary. */
export function invalidateAgentKnowledgeSourceSync(sessionId?: string): boolean {
  if (sessionId) return appliedKnowledgeSources.delete(sessionId);
  const hadEntries = appliedKnowledgeSources.size > 0;
  appliedKnowledgeSources.clear();
  return hadEntries;
}

/** Make the Runtime's MCP catalog match the task-owned source selection. */
export async function agentSetKnowledgeSources(
  sessionId: string,
  sources: KnowledgeSource[],
  organizationScopeIds: string[] = [],
): Promise<AgentKnowledgeSourcesResult> {
  const personal = sources.includes("personal");
  const organization = sources.includes("organization");
  const organizationError = organization
    ? organizationSelectionError(organizationScopeIds)
    : undefined;
  if (organizationError) {
    invalidateAgentKnowledgeSourceSync(sessionId);
    throw new Error(`组织知识库不可用：${organizationError}`);
  }
  const configurationKey = knowledgeConfigurationKey(sources, organizationScopeIds);
  // Any mutation attempt makes the previous acknowledgement unsafe to reuse.
  // A failed native transaction may also report that rollback failed.
  invalidateAgentKnowledgeSourceSync(sessionId);
  const result = await invoke<AgentKnowledgeSourcesResult>("agent_set_knowledge_sources", {
    sessionId,
    personal,
    organization,
    organizationScopeIds: organization ? organizationScopeIds : [],
  });
  const mismatches = [
    result.personalSelected !== personal ? "个人知识选择状态" : null,
    result.organizationSelected !== organization ? "组织知识选择状态" : null,
    result.personalAttached !== personal ? "个人知识库尚未就绪" : null,
    result.organizationAttached !== organization ? "组织知识库尚未就绪" : null,
  ].filter((label): label is string => Boolean(label));
  if (mismatches.length > 0) {
    throw new Error(`Runtime 未能确认知识来源状态：${mismatches.join("、")}`);
  }
  if (configurationKey !== knowledgeConfigurationKey(sources, organizationScopeIds)) {
    throw new Error("组织账号在知识来源同步期间发生变化，请重试");
  }
  appliedKnowledgeSources.set(sessionId, {
    key: configurationKey,
    result,
  });
  return result;
}

async function synchronizeKnowledgeSources(
  sessionId: string,
  promptId?: string,
): Promise<KnowledgeSource[]> {
  const store = useKnowledgeStore.getState();
  const sources = store.bindSessionSources(sessionId);
  const organizationScopeIds = organizationScopeIdsForSession(sessionId);
  const key = knowledgeConfigurationKey(sources, organizationScopeIds);
  const organizationPrerequisiteError = sources.includes("organization")
    ? organizationSelectionError(organizationScopeIds)
    : undefined;
  let result = !organizationPrerequisiteError && appliedKnowledgeSources.get(sessionId)?.key === key
    ? appliedKnowledgeSources.get(sessionId)?.result
    : undefined;
  let synchronizationError = organizationPrerequisiteError;
  if (!result && !synchronizationError) {
    try {
      result = await agentSetKnowledgeSources(sessionId, sources, organizationScopeIds);
    } catch (error) {
      synchronizationError = String(error).replace(/^Error:\s*/, "");
      console.warn("[EchoAgent] Knowledge source synchronization failed:", error);
    }
  }
  const unavailableLabels = [
    sources.includes("personal") && !result?.personalAttached ? "个人知识库" : null,
    sources.includes("organization") && !result?.organizationAttached ? "组织知识库" : null,
  ].filter((label): label is string => Boolean(label));
  const attachmentError = synchronizationError
    ?? (unavailableLabels.length > 0
      ? `${unavailableLabels.join("、")}尚未就绪`
      : undefined);

  if (attachmentError) {
    // Never silently broaden or narrow a user-selected knowledge boundary. A
    // failed detach may leave the previous tools resident, while a failed
    // attach would produce an ungrounded answer that looks knowledge-backed.
    invalidateAgentKnowledgeSourceSync(sessionId);
  }
  store.beginTurnTrace(
    sessionId,
    promptId,
    sources,
    sources.includes("organization")
      ? !attachmentError && result?.organizationAttached
        ? { state: "available" }
        : {
            state: "unavailable",
            message: `组织知识库不可用：${attachmentError ?? "服务尚未就绪"}`,
          }
      : undefined,
  );
  if (attachmentError) {
    if (sources.includes("personal")) {
      store.setRetrieval(sessionId, {
        state: "error",
        message: `知识工具连接失败：${attachmentError}`,
      }, promptId);
    }
    const guidance = organizationPrerequisiteError
      ? "请在组织工作台恢复登录或权限，并重新选择有效范围后再试"
      : "请重试；若持续失败，请完全退出后重启应用";
    throw new Error(
      `知识来源尚未就绪，本次消息未发送：${attachmentError}。${guidance}。`,
    );
  }
  return sources;
}

/** Validate all selected Runtime knowledge integrations before a destructive
 * conversation replacement. The subsequent send reuses this acknowledgement. */
export async function agentPrepareSend(sessionId: string): Promise<void> {
  await synchronizeKnowledgeSources(sessionId);
}

/** Send a user prompt; streamed updates arrive via the events below. */
export async function agentSend(
  sessionId: string,
  text: string,
  attachments: string[] = [],
  displayText: string = text,
  promptId?: string,
): Promise<void> {
  await synchronizeKnowledgeSources(sessionId, promptId);
  const prepared = await preparePromptWithPersonalKnowledge(
    sessionId,
    text,
    displayText,
    promptId,
  );
  await invoke<void>("agent_send", {
    sessionId,
    text: prepared.promptText,
    attachments,
    displayText,
    promptId: promptId ?? null,
    sendNow: false,
  });
}

/** Atomically cancel the active turn and submit its replacement. */
export async function agentSendNow(
  sessionId: string,
  text: string,
  attachments: string[] = [],
  displayText: string = text,
  promptId?: string,
): Promise<void> {
  await synchronizeKnowledgeSources(sessionId, promptId);
  const prepared = await preparePromptWithPersonalKnowledge(
    sessionId,
    text,
    displayText,
    promptId,
  );
  await invoke<void>("agent_send", {
    sessionId,
    text: prepared.promptText,
    attachments,
    displayText,
    promptId: promptId ?? null,
    sendNow: true,
  });
}

export async function agentCancel(
  sessionId: string,
  cancelAction: "pause" | "stop" = "stop",
  promptId?: string,
): Promise<void> {
  await invoke<void>("agent_cancel", {
    sessionId,
    cancelAction,
    promptId: promptId ?? null,
  });
}

/** Cleanly shut down the agent so `agentInit` can be called again to restart. */
export async function agentShutdown(): Promise<void> {
  invalidateAgentKnowledgeSourceSync();
  await invoke<void>("agent_shutdown");
}

/**
 * Rename a session via EchoAgent's `echo.agent/session/rename` extension method. EchoAgent
 * writes `generated_title` + `title_is_manual=true` to summary.json and
 * broadcasts `SessionSummaryGenerated`, which we also pick up via the
 * `agent://summary` event — so callers don't strictly need to optimistically
 * update the title, but doing so avoids a flicker while the event round-trips.
 *
 * `cwd` is optional but narrows EchoAgent's on-disk session lookup.
 */
export async function agentRenameSession(
  sessionId: string,
  title: string,
  cwd?: string,
): Promise<void> {
  await invoke<void>("agent_rename_session", { sessionId, title, cwd: cwd ?? null });
}

/**
 * Delete a session's persisted history via EchoAgent's `echo.agent/session/delete`.
 * Removes the on-disk session directory; the caller should drop the sidebar
 * entry on success.
 */
export interface SessionDeleteResult {
  memorySummariesDeleted: number;
  memoryCleanupWarning?: string | null;
}

export async function agentDeleteSession(
  sessionId: string,
  cwd?: string,
): Promise<SessionDeleteResult> {
  const result = await invoke<SessionDeleteResult>("agent_delete_session", {
    sessionId,
    cwd: cwd ?? null,
  });
  invalidateAgentKnowledgeSourceSync(sessionId);
  useKnowledgeStore.getState().forgetSession(sessionId);
  return result;
}

/**
 * Pin/unpin a session. EchoAgent's Summary has no pinned field, so this is
 * EchoAgent-only state stored in `~/.echo-agent/echoagent-state.json`. Returns the
 * new pinned value.
 */
export async function agentSetSessionPinned(
  sessionId: string,
  pinned: boolean,
): Promise<boolean> {
  return invoke<boolean>("agent_set_session_pinned", { sessionId, pinned });
}

/**
 * Archive/unarchive a session. EchoAgent's Summary has no archived field, so this
 * is EchoAgent-only state stored in `~/.echo-agent/echoagent-state.json`. Archived
 * sessions are hidden from the sidebar list. Returns the new archived value.
 */
export async function agentSetSessionArchived(
  sessionId: string,
  archived: boolean,
): Promise<boolean> {
  return invoke<boolean>("agent_set_session_archived", { sessionId, archived });
}

// ---------- context usage (echo.agent/session/info + echo.agent/session/usage) ----------

/**
 * Fetch the session's context-window snapshot (`echo.agent/session/info`) for the
 * composer's context-usage pill/popover. Rejects when the session isn't live
 * in the agent (e.g. an old session never loaded this launch) — callers
 * should treat that as "no data" and hide the pill.
 */
export async function agentSessionInfo(sessionId: string): Promise<SessionInfoResponse> {
  return invoke<SessionInfoResponse>("agent_session_info", { sessionId });
}

/**
 * Fetch the session's cumulative token usage (`echo.agent/session/usage`) — the
 * response wraps `PromptUsage` totals; we return the inner `usage` object.
 * Used by the context-usage popover for the average cache hit rate.
 */
export async function agentSessionUsage(sessionId: string): Promise<SessionUsage> {
  const resp = await invoke<{ usage: SessionUsage }>("agent_session_usage", { sessionId });
  return resp.usage;
}

export async function agentResolvePermission(
  requestId: string,
  outcome: { optionId?: string; cancelled?: boolean }
): Promise<boolean> {
  return invoke<boolean>("agent_resolve_permission", {
    requestId,
    optionId: outcome.optionId ?? null,
    cancelled: outcome.cancelled ?? false,
  });
}

export async function agentResolveQuestion(
  requestId: string,
  outcome: {
    /** Keyed by question text. Values are option labels (or string arrays for multi-select). */
    answers?: Record<string, string | string[]>;
    /** Per-question notes/preview, keyed by question text. Freeform uses notes. */
    annotations?: Record<string, { preview?: string; notes?: string }>;
    /** Exact AskUserQuestionExtResponse outcome. */
    outcome?: "accepted" | "cancelled" | "chat_about_this" | "skip_interview";
    /** Plan-mode partial answers (one display string per question). */
    partialAnswers?: Record<string, string>;
    cancelled?: boolean;
  }
): Promise<boolean> {
  return invoke<boolean>("agent_resolve_question", {
    requestId,
    answers: outcome.answers ?? null,
    annotations: outcome.annotations ?? null,
    partialAnswers: outcome.partialAnswers ?? null,
    outcome: outcome.outcome ?? null,
    cancelled: outcome.cancelled ?? false,
  });
}

export type PlanApprovalOutcome = "approved" | "cancelled" | "abandoned";

/** Resolve the exact parked exit-plan request; true is the only successful ACK. */
export async function agentResolvePlanApproval(
  requestId: string,
  outcome: PlanApprovalOutcome,
  feedback?: string,
): Promise<boolean> {
  return invoke<boolean>("agent_resolve_plan_approval", {
    requestId,
    outcome,
    feedback: feedback ?? null,
  });
}

// ---------- provider config (BYOK) ----------

export type ProviderKind =
  | "anthropic"
  | "openai"
  | "minimax"
  | "deepseek"
  | "qwen"
  | "custom"
  | "custom_anthropic";

/** API wire protocol. Mirrors EchoAgent's ApiBackend enum (snake_case). */
export type ApiBackend = "chat_completions" | "responses" | "messages";

/** HTTP auth header style. Mirrors EchoAgent's AuthScheme enum (snake_case). */
export type AuthScheme = "bearer" | "x_api_key";
export type ProviderSource = "personal" | "organization" | "builtin" | "legacy";

/**
 * One connection/auth profile — written to `[model_providers.<id>]`. A single
 * provider holds one api_key / base_url shared by every model that references
 * it via `providerId`.
 */
export interface ModelProviderEntry {
  /** Stable id derived from providerKind (e.g. "openai", "custom-2"). */
  id: string;
  providerKind: ProviderKind;
  label?: string;
  /** Masked "••••" when read back; the real secret when saving. */
  apiKey?: string;
  baseUrl?: string;
  /** Explicit approval to send a personal connection over plaintext HTTP. */
  allowInsecureHttp?: boolean;
  apiBackend?: ApiBackend;
  authScheme?: AuthScheme;
  /** Max context window in tokens, shared by all referencing models. */
  contextWindow?: number;
  source?: ProviderSource;
  managed?: boolean;
  credentialConfigured?: boolean;
  syncedAt?: number;
  organizationProvider?: string;
}

/**
 * One model catalog entry — written to `[model.<modelId>]` with a
 * `model_provider = "<providerId>"` reference. Carries only model-specific
 * fields; connection config lives on the provider.
 */
export interface ModelEntry {
  /** Stable local catalog key used by sessions and selectors. */
  modelId: string;
  /** Exact upstream model slug sent in requests. */
  remoteModelId?: string;
  /** References a ModelProviderEntry.id. */
  providerId: string;
  /** Human-readable display name (EchoAgent's `name` field). */
  name?: string;
  /** Per-model context-window override (wins over the provider's value). */
  contextWindow?: number;
  /** Per-model maximum response length, stored as max_completion_tokens. */
  maxOutputTokens?: number;
  /** Explicitly remove a saved maximum response length; omitted values keep it. */
  clearMaxOutputTokens?: boolean;
  managed?: boolean;
}

/** Result of providers_list: every provider + every model, joined by providerId. */
export interface ProviderListModel {
  providers: ModelProviderEntry[];
  models: ModelEntry[];
}

/**
 * Convenience: flatten the joined list back into per-model option rows for
 * pickers that only need { id, label }. Each model is joined with its
 * provider so consumers keep using a flat array.
 */
export interface ModelOptionRow {
  id: string;
  label: string;
  remoteModelId?: string;
  providerKind: ProviderKind;
  providerId: string;
  source?: ProviderSource;
  insecureHttp?: boolean;
}

/**
 * Restrict model options to the Runtime's effective catalog.
 *
 * `config.toml`'s `[models]` filters (`allowed_models`, `hidden_models`,
 * `disabled_models`) are applied inside the Runtime, so a model configured on
 * disk may legitimately not be selectable. The backend refuses those on send,
 * so offering them in the picker would produce an avoidable error.
 *
 * An empty `runtimeModels` means the catalog has not been read yet (init in
 * flight); the disk list is returned unchanged so the picker does not blank out.
 */
export function filterModelsByRuntimeCatalog(
  options: ModelOptionRow[],
  runtimeModels: string[],
): ModelOptionRow[] {
  // Upstream-branded ids never belong in the picker, including on the
  // mismatch fallback below — they come from the embedded Runtime's bundled
  // catalog, not from a connection the user configured, so selecting one
  // cannot succeed. See `model-branding`.
  const selectable = options.filter((option) => !isUpstreamBrandedModelId(option.id));
  if (runtimeModels.length === 0) return selectable;
  const catalog = new Set(runtimeModels);
  const allowed = selectable.filter((option) => catalog.has(option.id));
  // Never hand back an empty picker on an unexpected id mismatch — a visible
  // list that errors on send beats a list with nothing in it.
  return allowed.length > 0 ? allowed : selectable;
}

/** Flatten a ProviderListModel into per-model rows (id + label + provider). */
export function flattenModels(list: ProviderListModel): ModelOptionRow[] {
  const labels = new Map<string, number>();
  for (const model of list.models) {
    const label = model.name || model.remoteModelId || model.modelId;
    labels.set(label, (labels.get(label) ?? 0) + 1);
  }
  return list.models.map((m) => {
    const provider = list.providers.find((p) => p.id === m.providerId);
    const baseLabel = m.name || m.remoteModelId || m.modelId;
    const sourceLabel = provider?.source === "organization"
      ? "组织"
      : provider?.label || provider?.providerKind || m.providerId;
    return {
      id: m.modelId,
      label: (labels.get(baseLabel) ?? 0) > 1 ? `${baseLabel} · ${sourceLabel}` : baseLabel,
      remoteModelId: m.remoteModelId ?? m.modelId,
      providerKind: (provider?.providerKind ?? "custom") as ProviderKind,
      providerId: m.providerId,
      source: provider?.source,
      insecureHttp: provider?.baseUrl?.toLowerCase().startsWith("http://") ?? false,
    };
  });
}

export async function providersList(): Promise<ProviderListModel> {
  return invoke<ProviderListModel>("providers_list");
}

export async function providersSaveProvider(provider: ModelProviderEntry): Promise<void> {
  await invoke<void>("providers_save_provider", { provider });
}

export interface SaveConnectionResult {
  providerId: string;
  modelIds: string[];
}

export async function providersSaveConnection(
  provider: ModelProviderEntry,
  models: ModelEntry[],
  replaceModels = false,
): Promise<SaveConnectionResult> {
  return invoke<SaveConnectionResult>("providers_save_connection", {
    provider,
    models,
    replaceModels,
  });
}

export async function providersSaveModel(model: ModelEntry): Promise<string> {
  return invoke<string>("providers_save_model", { model });
}

export async function providersDeleteProvider(id: string): Promise<void> {
  await invoke<void>("providers_delete_provider", { id });
}

export async function providersDeleteModel(modelId: string): Promise<void> {
  await invoke<void>("providers_delete_model", { modelId });
}

/** One model entry returned by a provider's GET /models endpoint. */
export interface FetchedModel {
  id: string;
  ownedBy?: string;
}

/**
 * Fetch the list of available models from a provider's `/models` endpoint.
 * Works for any OpenAI-compatible endpoint and for Anthropic. The `apiKey` is
 * used only for this request — it is never persisted. Pass `baseUrl` to
 * override the provider's preset (required for `custom`).
 */
export async function providersFetchModels(
  providerKind: ProviderKind,
  apiKey: string,
  baseUrl?: string,
): Promise<FetchedModel[]> {
  return invoke<FetchedModel[]>("providers_fetch_models", {
    providerKind,
    apiKey,
    baseUrl: baseUrl ?? null,
  });
}

/** Discover models using the provider's native-side stored secret. */
export async function providersFetchModelsForProvider(providerId: string): Promise<FetchedModel[]> {
  return invoke<FetchedModel[]>("providers_fetch_models_for_provider", { providerId });
}

/** Test an unsaved draft; native code reuses the stored key when editing. */
export async function providersTestConnection(
  provider: ModelProviderEntry,
): Promise<FetchedModel[]> {
  return invoke<FetchedModel[]>("providers_test_connection", { provider });
}

/** Send a minimal real inference request for a concrete model id. */
export async function providersTestModelConnection(
  provider: ModelProviderEntry,
  modelId: string,
): Promise<void> {
  await invoke<void>("providers_test_model_connection", { provider, modelId });
}

// ---------- skills (echo.agent/skills/*) ----------

/** List all skills EchoAgent has discovered (user / project / bundled scopes). */
export async function skillsList(cwd?: string): Promise<SkillInfo[]> {
  return invoke<SkillInfo[]>("skills_list", { cwd: cwd ?? null });
}

/** Add a skill path (directory or file) to `[skills].paths` and rescan. */
export async function skillsAdd(path: string, cwd?: string): Promise<void> {
  await invoke<void>("skills_add", { path, cwd: cwd ?? null });
}

/** Remove a skill path from `[skills].paths`. */
export async function skillsRemove(path: string, cwd?: string): Promise<void> {
  await invoke<void>("skills_remove", { path, cwd: cwd ?? null });
}

/** Enable or disable a skill by name (writes `[skills] disabled`). */
export async function skillsToggle(name: string, enabled: boolean): Promise<void> {
  await invoke<void>("skills_toggle", { name, enabled });
}

/** Validate a folder, Markdown file, or ZIP without installing it. */
export async function skillsInspectPackage(path: string): Promise<SkillPackageInspection> {
  return invoke<SkillPackageInspection>("skills_inspect_package", { path });
}

/** Discover and inspect every independent Skill in a folder, Markdown file, or ZIP. */
export async function skillsInspectPackages(path: string): Promise<SkillPackageInspectionOutcome[]> {
  return invoke<SkillPackageInspectionOutcome[]>("skills_inspect_packages", { path });
}

/** Safely copy and atomically install/update a package under ~/.echo-agent/skills. */
export async function skillsInstallPackage(
  path: string,
  expectedSourceHash: string,
  approveHighRisk = false,
  packageRoot?: string,
): Promise<SkillInstallResult> {
  return invoke<SkillInstallResult>("skills_install_package", {
    path,
    expectedSourceHash,
    approveHighRisk,
    packageRoot: packageRoot ?? null,
  });
}

/** Remove an EchoAgent-managed package. External/project skills are never deleted. */
export async function skillsUninstallPackage(path: string): Promise<void> {
  await invoke<void>("skills_uninstall_package", { path });
}

// ---------- connectors / MCP (echo.agent/mcp/*) ----------

/** List configured MCP servers. Pass the live sessionId to enrich entries
 *  with session state (EchoAgent's list accepts it optionally). */
export async function mcpList(sessionId?: string, refresh = false): Promise<McpServerEntry[]> {
  return invoke<McpServerEntry[]>("mcp_list", { sessionId: sessionId ?? null, refresh });
}

/** Add or update an MCP server. Without a session it is persisted for the next
 *  session; with one it is also hot-applied. */
export async function mcpUpsert(
  sessionId: string | undefined,
  server: McpUpsertRequest,
): Promise<McpMutationResult> {
  return invoke<McpMutationResult>("mcp_upsert", { sessionId: sessionId ?? null, server });
}

/** Delete an MCP server by name. */
export async function mcpDelete(sessionId: string | undefined, name: string): Promise<McpMutationResult> {
  return invoke<McpMutationResult>("mcp_delete", { sessionId: sessionId ?? null, name });
}

/** Enable or disable an MCP server at runtime. */
export async function mcpToggle(
  sessionId: string | undefined,
  name: string,
  enabled: boolean,
): Promise<McpMutationResult> {
  return invoke<McpMutationResult>("mcp_toggle", { sessionId: sessionId ?? null, name, enabled });
}

/** Complete a Runtime-provided connector setup schema in a live session. */
export async function mcpSetup(
  sessionId: string,
  name: string,
  values: Record<string, string>,
): Promise<void> {
  await invoke<void>("mcp_setup", { sessionId, name, values });
}

/** Enable or disable one MCP tool in the active session. */
export async function mcpToggleTool(
  sessionId: string,
  serverName: string,
  toolName: string,
  enabled: boolean,
): Promise<void> {
  await invoke<void>("mcp_toggle_tool", { sessionId, serverName, toolName, enabled });
}

/** Resolved absolute path of the standalone mcp.json (for the editor header). */
export async function mcpConfigPath(): Promise<string> {
  return invoke<string>("mcp_config_path");
}

/** Read the standalone mcp.json (returns an empty template if missing). */
export async function mcpConfigRead(): Promise<McpConfigFile> {
  return invoke<McpConfigFile>("mcp_config_read");
}

/** Validate + write the standalone mcp.json. When a sessionId is given each
 *  server is also synced live into EchoAgent (its upsert is session-scoped). */
export async function mcpConfigSave(content: string, sessionId?: string): Promise<McpConfigSaveResult> {
  return invoke<McpConfigSaveResult>("mcp_config_save", { content, sessionId: sessionId ?? null });
}

// ---------- MCP OAuth authorization (echo.agent/mcp/auth_*) ----------

/** Kick off EchoAgent's browser OAuth flow for one MCP server. EchoAgent opens the
 *  system browser itself and the call resolves when the flow completes
 *  (status "authenticated" | "failed" | "setup_required"). */
export async function mcpAuthTrigger(
  sessionId: string,
  serverName: string,
): Promise<McpAuthTriggerResult> {
  return invoke<McpAuthTriggerResult>("mcp_auth_trigger", { sessionId, serverName });
}

/** List servers EchoAgent has flagged `needs_auth` for this session. */
export async function mcpAuthStatus(sessionId: string): Promise<McpAuthStatusEntry[]> {
  return invoke<McpAuthStatusEntry[]>("mcp_auth_status", { sessionId });
}

/** Subscribe to Runtime MCP handshake/health changes. */
export function onMcpStatusEvent(
  cb: (payload: McpServerStatusEvent) => void,
): Promise<UnlistenFn> {
  return listen<McpServerStatusEvent>("agent://mcp-status", (event) => cb(event.payload));
}

// ---------- CLI-type connector authorization (cli.json driven) ----------

/** Probe a CLI connector: has cli.json / CLI installed / currently authed. */
export async function connectorsCliStatus(
  root: string,
  source: string,
): Promise<ConnectorCliStatus> {
  return invoke<ConnectorCliStatus>("connectors_cli_status", { root, source });
}

/** Run the full CLI authorization flow (install → auth steps → verify).
 *  Long-running; auth URLs arrive via `onConnectorCliAuthUrl`. */
export async function connectorsCliAuth(
  root: string,
  source: string,
): Promise<ConnectorCliAuthResult> {
  return invoke<ConnectorCliAuthResult>("connectors_cli_auth", { root, source });
}

/** Cancel an in-flight CLI authorization (kills the child process tree). */
export async function connectorsCliAuthCancel(source: string): Promise<void> {
  await invoke<void>("connectors_cli_auth_cancel", { source });
}

/** Run the connector's unAuth command (logout / credential wipe). */
export async function connectorsCliUnauth(root: string, source: string): Promise<void> {
  await invoke<void>("connectors_cli_unauth", { root, source });
}

/** Absolute path of the connector's bundled skills/ dir (null if none). */
export async function connectorsCliSkillsDir(
  root: string,
  source: string,
): Promise<string | null> {
  return invoke<string | null>("connectors_cli_skills_dir", { root, source });
}

/** Subscribe to CLI auth URL events (show QR / open browser). */
export function onConnectorCliAuthUrl(
  cb: (e: ConnectorCliAuthUrlEvent) => void,
): Promise<UnlistenFn> {
  return listen<ConnectorCliAuthUrlEvent>("connector://cli-auth-url", (ev) => cb(ev.payload));
}

/** Subscribe to CLI auth log lines (progress display in the QR modal). */
export function onConnectorCliAuthLog(
  cb: (e: ConnectorCliAuthLogEvent) => void,
): Promise<UnlistenFn> {
  return listen<ConnectorCliAuthLogEvent>("connector://cli-auth-log", (ev) => cb(ev.payload));
}

/** Subscribe to CLI auth completion events. */
export function onConnectorCliAuthDone(
  cb: (e: ConnectorCliAuthDoneEvent) => void,
): Promise<UnlistenFn> {
  return listen<ConnectorCliAuthDoneEvent>("connector://cli-auth-done", (ev) => cb(ev.payload));
}

// ---------- connector marketplace (live local data dir) ----------

/** First existing candidate marketplace root ("" if none found). */
export async function connectorsDefaultRoot(): Promise<string> {
  return invoke<string>("connectors_default_root");
}

/** Marketplace roots under `root` that contain the connectors manifest. */
export async function connectorsListRoots(root: string): Promise<string[]> {
  return invoke<string[]>("connectors_list_roots", { root });
}

/** Load categories + connectors from the marketplace manifest. */
export async function connectorsLoad(root?: string): Promise<ConnectorCatalog> {
  return invoke<ConnectorCatalog>("connectors_load", { root: root ?? null });
}

/** Read a local icon file as a `data:` URL (svg/png). */
export async function connectorsIcon(path: string): Promise<string> {
  return invoke<string>("connectors_icon", { path });
}

/** Read `<root>/connectors/<source>/mcp.json` raw text ("" if missing). */
export async function connectorsReadMcpConfig(root: string, source: string): Promise<string> {
  return invoke<string>("connectors_read_mcp_config", { root, source });
}

/** Open a URL in the system browser (scheme-whitelisted backend command). */
export async function openUrl(url: string): Promise<void> {
  await invoke<void>("open_url", { url });
}

// ---------- skill catalog (runtime scan of agents + builtin dirs) ----------

/** First existing candidate agents data root ("" if none found). */
export async function skillsCatalogDefaultRoot(): Promise<string> {
  return invoke<string>("skills_catalog_default_root");
}

/** Agents roots under `root` that look scannable. */
export async function skillsCatalogListRoots(root: string): Promise<string[]> {
  return invoke<string[]>("skills_catalog_list_roots", { root });
}

/** Scan both sources and return the merged, deduped skill catalog. */
export async function skillsCatalogLoad(
  root?: string,
  builtinRoot?: string,
): Promise<SkillCatalog> {
  return invoke<SkillCatalog>("skills_catalog_load", {
    root: root ?? null,
    builtinRoot: builtinRoot ?? null,
  });
}

/** Read the full SKILL.md text for a directory. */
export async function skillsCatalogReadSkill(dir: string): Promise<string> {
  return invoke<string>("skills_catalog_read_skill", { dir });
}

// ---------- expert marketplace (live local data dir) ----------

/** First existing candidate data root ("" if none found). */
export async function expertsDefaultRoot(): Promise<string> {
  return invoke<string>("experts_default_root");
}

/** Data roots under `root` that contain the marketplace manifest. */
export async function expertsListRoots(root: string): Promise<string[]> {
  return invoke<string[]>("experts_list_roots", { root });
}

/** Load categories + experts by merging the manifest with each plugin.json. */
export async function expertsLoad(root?: string): Promise<ExpertCatalog> {
  return invoke<ExpertCatalog>("experts_load", { root: root ?? null });
}

/** Small base64 JPEG thumbnail for a local avatar path (cached server-side). */
export async function expertsThumbnail(path: string): Promise<string> {
  return invoke<string>("experts_thumbnail", { path });
}

/** Full-size local image as a `data:` URL (used for 精选场景 banners). */
export async function expertsImageBytes(path: string): Promise<string> {
  return invoke<string>("experts_image_bytes", { path });
}

/** Read the full agent prompt markdown from an expert's package directory. */
export async function expertsReadAgentPrompt(
  root: string,
  plugin: string,
  agentName: string,
): Promise<string> {
  return invoke<string>("experts_read_agent_prompt", { root, plugin, agentName });
}

/** Link a team expert's agents/*.md into ~/.echo-agent/agents/ for EchoAgent sub-agent discovery. */
export async function expertsLinkAgents(root: string, plugin: string): Promise<number> {
  return invoke<number>("experts_link_agents", { root, plugin });
}

/** Bind an expert to a session (EchoAgent-only state). */
export async function agentSetSessionExpert(
  sessionId: string,
  expertId: string,
  expertName: string,
  source: string,
  avatarLocal?: string,
): Promise<boolean> {
  return invoke<boolean>("agent_set_session_expert", { sessionId, expertId, expertName, source, avatarLocal: avatarLocal ?? null });
}

/** Remove the expert binding from a session. */
export async function agentClearSessionExpert(sessionId: string): Promise<boolean> {
  return invoke<boolean>("agent_clear_session_expert", { sessionId });
}

// ---------- experts / assistants (~/.echo-agent/agents/*.md) ----------

/** List all agent definitions visible to EchoAgent. */
export async function agentsList(cwd?: string): Promise<AgentEntry[]> {
  return invoke<AgentEntry[]>("agents_list", { cwd: cwd ?? null });
}

/** Fetch a single agent file's full contents. */
export async function agentsGet(path: string): Promise<string> {
  return invoke<string>("agents_get", { path });
}

/** Save an agent file (create or overwrite) to ~/.echo-agent/agents/<name>.md. */
export async function agentsSave(name: string, raw: string): Promise<AgentEntry> {
  return invoke<AgentEntry>("agents_save", { name, raw });
}

/** Delete an agent file by path. */
export async function agentsDelete(path: string): Promise<void> {
  await invoke<void>("agents_delete", { path });
}

/** Render a starter agent markdown body from name/description/system prompt.
 *  Optional avatar (1-20) and modelTags are written to frontmatter. */
export async function agentsTemplate(
  name: string,
  description: string,
  systemPrompt: string,
  avatar?: number,
  modelTags?: string[],
): Promise<string> {
  return invoke<string>("agents_template", {
    name,
    description,
    systemPrompt,
    avatar: avatar ?? null,
    modelTags: modelTags ?? null,
  });
}

// ---------- permission rules (~/.echo-agent/config.toml [permission]) ----------

/** List the current permission rules (allow/deny/ask) from config.toml. */
export async function permissionList(): Promise<PermissionRule[]> {
  return invoke<PermissionRule[]>("permission_list");
}

/** Replace all permission rules. Writes to config.toml atomically.
 *  NOTE: requires a EchoAgent restart to take effect. */
export async function permissionSave(rules: PermissionRule[]): Promise<void> {
  await invoke<void>("permission_save", { rules });
}

// ---------- task-scoped permission mode ----------

/** EchoAgent 的权限模式:审批(ask)/自动(auto)/始终允许(always-approve)。 */
export type PermissionMode = "ask" | "auto" | "always-approve";

export interface PermissionModeStatus {
  /** Present for an existing task; absent for the home/new-task capability read. */
  sessionId?: string;
  /** Effective mode the Runtime can honor now. */
  permissionMode: PermissionMode;
  /** Configured or policy-selected mode before Auto is clamped to Ask. */
  configuredPermissionMode: PermissionMode;
  autoModeAvailable: boolean;
  autoModeUnavailableReason?: string;
  alwaysApproveAvailable: boolean;
  alwaysApproveUnavailableReason?: string;
  locked: boolean;
  lockedReason?: string;
  runtimeSyncState: "offline" | "syncing" | "synced" | "failed";
  runtimeAppliedMode?: PermissionMode;
  runtimeSyncError?: string;
}

export interface PermissionModeSetResult extends PermissionModeStatus {
  agentRunning: boolean;
  runtimeSynced: boolean;
  resolvedPending: number;
  remainingPending: number;
  resolvedPermissions: PermissionClosedEvent[];
}

/** Read one task's mode, or safe new-task capabilities when no id is supplied. */
export async function permissionModeGet(sessionId?: string): Promise<PermissionModeStatus> {
  return invoke<PermissionModeStatus>("permission_mode_get", { sessionId: sessionId ?? null });
}

/** Set one existing task's mode and live-notify only that Runtime session. */
export async function permissionModeSet(
  sessionId: string,
  mode: PermissionMode,
): Promise<PermissionModeSetResult> {
  return invoke<PermissionModeSetResult>("permission_mode_set", { sessionId, mode });
}

// ---------- memory (资料库 — Runtime canonical storage) ----------

export interface MemoryConfig {
  enabled: boolean;
  initialInjectionEnabled: boolean;
  saveOnEnd: boolean;
  watcherEnabled: boolean;
  autoFlushEnabled: boolean;
  dreamEnabled: boolean;
  revision?: string;
}

export async function memoryConfigGet(): Promise<MemoryConfig> {
  return invoke<MemoryConfig>("memory_config_get");
}

export async function memoryConfigSave(memory: Partial<MemoryConfig>, expectedRevision?: string): Promise<MemoryConfig> {
  return invoke<MemoryConfig>("memory_config_save", { memory, expectedRevision });
}

/** List memory notes from global + workspace scope. */
export async function memoryList(cwd?: string): Promise<MemoryEntry[]> {
  return invoke<MemoryEntry[]>("memory_list", { cwd: cwd ?? null });
}

/** Read a single memory file. */
export async function memoryGet(scope: string, path: string, cwd?: string): Promise<string> {
  return invoke<string>("memory_get", { scope, path, cwd: cwd ?? null });
}

/** Create or overwrite a memory note. */
export async function memorySave(
  scope: string,
  path: string,
  content: string,
  cwd?: string,
  expectedRevision?: string,
): Promise<MemoryEntry> {
  return invoke<MemoryEntry>("memory_save", {
    scope,
    path,
    content,
    cwd: cwd ?? null,
    expectedRevision: expectedRevision ?? null,
  });
}

/** Append one note to the selected canonical MEMORY.md. */
export async function memoryAppend(
  scope: "global" | "workspace",
  content: string,
  cwd?: string,
): Promise<MemoryEntry> {
  return invoke<MemoryEntry>("memory_append", { scope, content, cwd: cwd ?? null });
}

/** Delete a memory note. */
export async function memoryDelete(
  scope: string,
  path: string,
  cwd?: string,
  expectedRevision?: string,
): Promise<void> {
  await invoke<void>("memory_delete", {
    scope,
    path,
    cwd: cwd ?? null,
    expectedRevision: expectedRevision ?? null,
  });
}

/** Delete generated session summaries, archives and index entries for a workspace. */
export async function memoryClearSessionSummaries(cwd: string): Promise<number> {
  return invoke<number>("memory_clear_session_summaries", { cwd });
}

/** Rewrite an editor buffer using the active session model; does not save it. */
export async function memoryRewrite(
  sessionId: string,
  rawText: string,
  contextSummary: string,
): Promise<string> {
  return invoke<string>("memory_rewrite", { sessionId, rawText, contextSummary });
}

/** Flush in-flight memory writes to disk (`echo.agent/memory/flush`). */
export async function memoryFlush(sessionId: string): Promise<boolean> {
  return invoke<boolean>("memory_flush", { sessionId });
}

/** Consolidate generated session summaries into long-term memory. */
export async function memoryDream(sessionId: string): Promise<boolean> {
  return invoke<boolean>("memory_dream", { sessionId });
}

// ---------- session search (FTS5) ----------

/** Full-text search across all sessions. */
export async function sessionSearch(
  query: string,
  cwd?: string,
  limit?: number,
): Promise<SearchHit[]> {
  return invoke<SearchHit[]>("session_search", { query, cwd: cwd ?? null, limit: limit ?? null });
}

// ---------- rewind ----------

/** List prompts a session can rewind to. */
export async function rewindPoints(sessionId: string): Promise<RewindPoint[]> {
  return invoke<RewindPoint[]>("rewind_points", { sessionId });
}

/** Rewind a session to a specific prompt index. */
export async function rewindExecute(
  sessionId: string,
  targetPromptIndex: number,
  mode: RewindMode,
  force = false,
): Promise<RewindExecution> {
  return invoke<RewindExecution>("rewind_execute", {
    sessionId,
    targetPromptIndex,
    mode,
    force,
  });
}

// ---------- session fork ----------

/** Fork a session: copy history to a new session id. Returns the new id. */
export async function sessionFork(sessionId: string, cwd?: string): Promise<string> {
  return invoke<string>("session_fork", { sessionId, cwd: cwd ?? null });
}

// ---------- slash commands + prompt history ----------

/**
 * List slash commands (builtin + skills + plugins/workflows). A live session
 * yields the authoritative capability-gated catalog; cwd keeps project Skills
 * and Plugins discoverable before the first session is created.
 */
export async function commandsList(sessionId?: string, cwd?: string): Promise<SlashCommand[]> {
  return invoke<SlashCommand[]>("commands_list", {
    sessionId: sessionId ?? null,
    cwd: cwd ?? null,
  });
}

/** Cross-session prompt history. */
export async function promptHistory(limit?: number): Promise<string[]> {
  return invoke<string[]>("prompt_history", { limit: limit ?? null });
}

// ---------- tasks / subagents ----------

/** List running background tasks / subagents owned by one live session. */
export async function tasksList(sessionId: string): Promise<RunningTask[]> {
  return invoke<RunningTask[]>("tasks_list", { sessionId });
}

/** Kill a running task or subagent. */
export async function taskKill(
  sessionId: string,
  taskId: string,
  source: RunningTask["source"],
): Promise<void> {
  await invoke<void>("task_kill", { sessionId, taskId, source });
}

export interface RuntimeTeamInfo {
  teamId: string;
  members: string[];
  createdAt: number;
}

/** Authoritative teams persisted by the embedded team MCP runtime. */
export async function teamSnapshot(): Promise<RuntimeTeamInfo[]> {
  return invoke<RuntimeTeamInfo[]>("team_snapshot");
}

// ---------- folder trust ----------

export interface FolderTrustRequest {
  requestId: string;
  sessionId: string;
  cwd: string;
  workspace: string;
  configKinds: string[];
}

export interface PendingInteractions {
  permissions: PermissionRequest[];
  questions: QuestionRequest[];
  planApprovals: PlanApprovalRequest[];
  folderTrustRequests: FolderTrustRequest[];
}

export interface QuestionClosedEvent {
  requestId: string;
  sessionId: string;
}

export interface PermissionClosedEvent {
  requestId: string;
  sessionId: string;
}

/** Observe authoritative closure (answer, timeout, disconnect) of a question. */
export async function onQuestionClosedEvent(
  callback: (event: QuestionClosedEvent) => void,
): Promise<UnlistenFn> {
  return listen<QuestionClosedEvent>("agent://question-closed", (event) => {
    callback(event.payload);
  });
}

/** Replay all unresolved reverse requests, optionally scoped to one session. */
export async function agentListPendingInteractions(
  sessionId?: string,
): Promise<PendingInteractions> {
  return invoke<PendingInteractions>("agent_list_pending_interactions", {
    sessionId: sessionId ?? null,
  });
}

/** Respond on the original folder-trust ExtMethod channel. */
export async function folderTrustRespond(
  requestId: string,
  trusted: boolean,
): Promise<boolean> {
  return invoke<boolean>("folder_trust_respond", { requestId, trusted });
}

// ---------- plan mode ----------

/** Select EchoAgent's native Ask / Plan / Agent prompt mode. */
export async function setCodingMode(
  sessionId: string,
  mode: "ask" | "plan" | "agent" | "browser_use" | "computer_use",
): Promise<void> {
  await invoke<void>("set_coding_mode", { sessionId, mode });
}

/** Idempotently set plan mode; CurrentModeUpdate is the authoritative result. */
export async function setPlanMode(sessionId: string, enabled: boolean): Promise<void> {
  await invoke<void>("set_plan_mode", { sessionId, enabled });
}

/** @deprecated Kept for older callers; behavior is now idempotent set. */
export async function togglePlanMode(sessionId: string, enabled: boolean): Promise<void> {
  await setPlanMode(sessionId, enabled);
}

// ---------- internal reload ----------

/** Hot-reload EchoAgent's view of config/skills/mcp/models. `kind` ∈
 *  "mcp_all" | "mcp_project" | "skills" | "models". */
export async function internalReload(kind: "mcp_all" | "mcp_project" | "skills" | "models"): Promise<void> {
  await invoke<void>("internal_reload", { kind });
}

// ---------- authoritative local policy ----------

export async function policyGet<T>(): Promise<T> {
  return invoke<T>("policy_get");
}

export async function policySave<T>(policy: T): Promise<T> {
  return invoke<T>("policy_save", { policy });
}

// ---------- automations (local scheduler, EchoAgent 1:1) ----------

/** Full snapshot: automations (next runs recomputed) + run records. */
export async function automationsSnapshot(): Promise<AutomationSnapshot> {
  return invoke<AutomationSnapshot>("automations_snapshot");
}

/** Create or update an automation. */
export async function automationsSave(automation: Automation): Promise<Automation> {
  return invoke<Automation>("automations_save", { automation });
}

/** Delete an automation by id. */
export async function automationsDelete(id: string): Promise<void> {
  await invoke<void>("automations_delete", { id });
}

/** Set an automation's status ("ACTIVE" | "PAUSED"). */
export async function automationsSetStatus(id: string, status: AutomationStatus): Promise<void> {
  await invoke<void>("automations_set_status", { id, status });
}

/** Queue an automation run and return its durable record id immediately. */
export async function automationsRun(id: string): Promise<string> {
  return invoke<string>("automations_run", { id });
}

/** Archive / unarchive a run record. */
export async function automationRecordsArchive(id: string, archived: boolean): Promise<void> {
  await invoke<void>("automation_records_archive", { id, archived });
}

/** Delete a run record. */
export async function automationRecordsDelete(id: string): Promise<void> {
  await invoke<void>("automation_records_delete", { id });
}

// ---------- agent / assistant defaults (~/.echo-agent/config.toml) ----------

/** Read the new-session defaults (model + permission + remember-tool-approvals). */
export async function agentsDefaultsGet(): Promise<AgentDefaults> {
  return invoke<AgentDefaults>("agents_defaults_get");
}

/** Save the new-session defaults. Atomic write to config.toml. */
export async function agentsDefaultsSave(defaults: AgentDefaults): Promise<void> {
  await invoke<void>("agents_defaults_save", { defaults });
}

// ---------- subagents config (~/.echo-agent/config.toml [subagents]) ----------

/** `[subagents]` config — currently exposes `max_depth` (nesting depth). */
export interface SubagentsConfig {
  /** Maximum subagent nesting depth (≥1). EchoAgent default = 1. */
  maxDepth: number;
}

/** Read `[subagents].max_depth`. Returns 1 when unset. */
export async function subagentsConfigGet(): Promise<SubagentsConfig> {
  return invoke<SubagentsConfig>("subagents_config_get");
}

/** Write `[subagents].max_depth` (clamped ≥1). Requires agent restart. */
export async function subagentsConfigSave(maxDepth: number): Promise<number> {
  return invoke<number>("subagents_config_save", { maxDepth });
}

// ---------- web search config (~/.echo-agent/config.toml [models].web_search) ----------

/** `[models].web_search` config — derived enabled flag + model id. */
export interface WebSearchConfig {
  /** true when a web_search model is set. */
  enabled: boolean;
  /** Configured web_search model id (empty = none). */
  model: string;
}

/** Read the web_search model. `enabled` is derived from whether a model is set. */
export async function webSearchConfigGet(): Promise<WebSearchConfig> {
  return invoke<WebSearchConfig>("web_search_config_get");
}

/** Enable/disable web search by setting/clearing `[models].web_search`.
 *  When enabling, `model` must be a non-empty model id. Requires agent restart. */
export async function webSearchConfigSave(
  enable: boolean,
  model?: string,
): Promise<boolean> {
  return invoke<boolean>("web_search_config_save", { enable, model });
}

// ---------- plugins + marketplace (echo.agent/plugins/*, echo.agent/marketplace/*) ----------

import type {
  MarketplaceListResponse,
  PluginsListResponse,
} from "./types";

/** List installed plugins via `echo.agent/plugins/list`. */
export async function pluginsList(sessionId?: string): Promise<PluginsListResponse> {
  return invoke<PluginsListResponse>("plugins_list", { sessionId: sessionId ?? null });
}

/** Execute a plugin action (enable/disable/install/etc). */
export async function pluginsAction(
  sessionId: string,
  action: unknown,
): Promise<unknown> {
  return invoke("plugins_action", { sessionId, action });
}

/** List marketplace sources + plugins via `echo.agent/marketplace/list`. */
export async function marketplaceList(sessionId?: string): Promise<MarketplaceListResponse> {
  return invoke<MarketplaceListResponse>("marketplace_list", { sessionId: sessionId ?? null });
}

/** Execute a marketplace action (install/uninstall/refresh/add_source/remove_source). */
export async function marketplaceAction(
  sessionId: string,
  action: unknown,
): Promise<unknown> {
  return invoke("marketplace_action", { sessionId, action });
}

// ---------- notification center ----------

import type { NotificationEntry, NotificationKind } from "./types";

/** Append a notification to the log (called when a EchoAgent event is received). */
export async function notificationAppend(
  kind: NotificationKind | string,
  title: string,
  body?: string,
  sessionId?: string,
  severity?: "info" | "warn" | "error",
): Promise<void> {
  await invoke<void>("notification_append", {
    kind,
    title,
    body: body ?? null,
    sessionId: sessionId ?? null,
    severity: severity ?? null,
  });
}

/** List notifications (newest first). */
export async function notificationList(): Promise<NotificationEntry[]> {
  return invoke<NotificationEntry[]>("notification_list");
}

/** Mark a notification as read. */
export async function notificationMarkRead(id: number): Promise<void> {
  await invoke<void>("notification_mark_read", { id });
}

/** Mark all as read. */
export async function notificationMarkAllRead(): Promise<void> {
  await invoke<void>("notification_mark_all_read");
}

/** Clear all notifications. */
export async function notificationClear(): Promise<void> {
  await invoke<void>("notification_clear");
}

// ---------- export ----------

/** Select and authorize a local directory in one native backend operation. */
export async function filesystemPickDirectory(suggestedPath?: string): Promise<string | null> {
  return suggestedPath
    ? invoke<string | null>("filesystem_pick_directory", { suggestedPath })
    : invoke<string | null>("filesystem_pick_directory");
}

/** Read a bounded UTF-8 text file from an authorized workspace. */
export async function readTextFile(
  path: string,
  cwd?: string,
  maxBytes = 2 * 1024 * 1024,
): Promise<string> {
  return invoke<string>("read_text_file", { path, cwd: cwd ?? null, maxBytes });
}

/** Atomically write a text file below the selected workspace root. */
export async function writeTextFile(
  path: string,
  content: string,
  workspaceRoot: string,
): Promise<string> {
  return invoke<string>("write_text_file", { path, content, workspaceRoot });
}

// ---------- dedicated Coding Workspace ----------

export interface CodingLanguageStat {
  language: string;
  files: number;
}

export interface CodingModuleInfo {
  name: string;
  path: string;
  kind: string;
  dependencies: string[];
}

export interface CodingWorkspaceAnalysis {
  root: string;
  name: string;
  projectType: string;
  fileCount: number;
  truncated: boolean;
  languages: CodingLanguageStat[];
  modules: CodingModuleInfo[];
  validationCommands: string[];
  hasGit: boolean;
  gitBranch?: string;
  gitChangedFiles: number;
  instructionFiles: string[];
  scannedAt: string;
}

export type CodingGitFileStatus = "modified" | "added" | "deleted" | "renamed" | "untracked" | "conflict" | "ignored";

export interface CodingGitFile {
  path: string;
  oldPath?: string;
  status: CodingGitFileStatus;
  staged: boolean;
  unstaged: boolean;
  untracked: boolean;
  added: number;
  removed: number;
}

export interface CodingGitSnapshot {
  hasGit: boolean;
  branch?: string;
  head?: string;
  files: CodingGitFile[];
  totalAdded: number;
  totalRemoved: number;
  capturedAt: string;
}

export interface CodingDocument {
  path: string;
  relativePath: string;
  content: string;
  hash: string;
  size: number;
  modifiedAt: number;
  language: string;
  lineEnding: "LF" | "CRLF";
}

export interface CodingSearchHit {
  path: string;
  line: number;
  column: number;
  preview: string;
}

export interface CodingSearchOptions {
  caseSensitive?: boolean;
  wholeWord?: boolean;
  regex?: boolean;
  includeGlob?: string;
  excludeGlob?: string;
}

export interface CodingTerminalEvent {
  terminalId: string;
  dataBase64: string;
}

export async function codingAnalyzeWorkspace(root: string): Promise<CodingWorkspaceAnalysis> {
  return invoke<CodingWorkspaceAnalysis>("coding_analyze_workspace", { root });
}

export async function codingGitSnapshot(root: string): Promise<CodingGitSnapshot> {
  return invoke<CodingGitSnapshot>("coding_git_snapshot", { root });
}

export async function codingGitDiff(root: string, path?: string): Promise<string> {
  return invoke<string>("coding_git_diff", { root, path: path ?? null });
}

export async function codingGitSetStaged(
  root: string,
  path: string,
  staged: boolean,
): Promise<CodingGitSnapshot> {
  return invoke<CodingGitSnapshot>("coding_git_set_staged", { root, path, staged });
}

export async function codingReadDocument(root: string, path: string): Promise<CodingDocument> {
  return invoke<CodingDocument>("coding_read_document", { root, path });
}

export async function codingWriteDocument(
  root: string,
  path: string,
  content: string,
  expectedHash: string,
): Promise<CodingDocument> {
  return invoke<CodingDocument>("coding_write_document", {
    request: { root, path, content, expectedHash },
  });
}

export async function codingCreateEntry(
  root: string,
  parent: string,
  name: string,
  directory: boolean,
): Promise<string> {
  return invoke<string>("coding_create_entry", {
    request: { root, parent, name, directory },
  });
}

export interface CodingBatchOpResult {
  /** Result path on success; source path on failure. */
  path: string;
  /** Original source path for stable partial-batch correlation. */
  sourcePath: string;
  ok: boolean;
  error?: string | null;
  /** Opaque system-trash restore token, present only after a successful delete. */
  restoreToken?: string | null;
}

export interface CodingRenameResult {
  path: string;
  oldPath: string;
}

export async function codingDeleteEntries(
  root: string,
  paths: string[],
): Promise<CodingBatchOpResult[]> {
  return invoke<CodingBatchOpResult[]>("coding_delete_entries", {
    request: { root, paths },
  });
}

export async function codingRenameEntry(
  root: string,
  path: string,
  newName: string,
): Promise<CodingRenameResult> {
  return invoke<CodingRenameResult>("coding_rename_entry", {
    request: { root, path, newName },
  });
}

export async function codingCopyEntries(
  root: string,
  sources: string[],
  destination: string,
): Promise<CodingBatchOpResult[]> {
  return invoke<CodingBatchOpResult[]>("coding_copy_entries", {
    request: { root, sources, destination },
  });
}

export async function codingMoveEntries(
  root: string,
  sources: string[],
  destination: string,
): Promise<CodingBatchOpResult[]> {
  return invoke<CodingBatchOpResult[]>("coding_move_entries", {
    request: { root, sources, destination },
  });
}

export interface CodingRestoreResult {
  path: string;
  ok: boolean;
  error?: string | null;
}

export async function codingRestoreFromTrash(
  root: string,
  originalPaths: string[],
  restoreTokens: string[],
): Promise<CodingRestoreResult[]> {
  return invoke<CodingRestoreResult[]>("coding_restore_from_trash", {
    request: { root, originalPaths, restoreTokens },
  });
}

export async function codingSearchWorkspace(
  root: string,
  query: string,
  options: CodingSearchOptions = {},
): Promise<CodingSearchHit[]> {
  return invoke<CodingSearchHit[]>("coding_search_workspace", { root, query, options });
}

export async function codingTerminalCreate(
  root: string,
  cols: number,
  rows: number,
): Promise<string> {
  const result = await invoke<{ terminalId: string }>("coding_terminal_create", { root, cols, rows });
  return result.terminalId;
}

export async function codingTerminalWrite(terminalId: string, data: string): Promise<void> {
  await invoke<void>("coding_terminal_write", { terminalId, data });
}

export async function codingTerminalResize(
  terminalId: string,
  cols: number,
  rows: number,
): Promise<void> {
  await invoke<void>("coding_terminal_resize", { terminalId, cols, rows });
}

export async function codingTerminalClose(terminalId: string): Promise<boolean> {
  return invoke<boolean>("coding_terminal_close", { terminalId });
}

export function codingListenTerminalOutput(
  callback: (event: CodingTerminalEvent) => void,
): Promise<UnlistenFn> {
  return listen<CodingTerminalEvent>("coding://terminal-output", (event) => callback(event.payload));
}

export function codingListenTerminalExit(
  callback: (event: CodingTerminalEvent) => void,
): Promise<UnlistenFn> {
  return listen<CodingTerminalEvent>("coding://terminal-exit", (event) => callback(event.payload));
}

/** Show the backend-owned native save dialog and export to its exact result.
 * The renderer never supplies the destination path, preventing a forged IPC
 * request from turning export into an arbitrary-file overwrite primitive. */
export async function exportTextFile(
  suggestedName: string,
  content: string,
  extension: string,
): Promise<string | null> {
  return invoke<string | null>("export_text_file", { suggestedName, extension, content });
}

// ---------- filesystem: directory listing (file-tree sidebar) ----------

/** A single directory entry returned by `list_dir`. */
export interface DirEntry {
  /** File/dir basename. */
  name: string;
  /** Absolute path of the entry. */
  path: string;
  /** "directory" | "file" | "other" | "symlink" (SP5). */
  kind: string;
  /** File size in bytes (directories report 0). */
  size: number;
  /** Last modified time in Unix milliseconds (0 when unavailable). */
  modifiedAt: number;
  /** SP5: file size exceeds 2 MiB. */
  isLarge: boolean;
  /** SP5: extension whitelist or first 8 KiB contain a NUL byte. */
  isBinary: boolean;
}

/**
 * List the immediate children of a directory (non-recursive).
 * Hidden entries and noisy build/VCS directories (.git/node_modules/…) are
 * skipped server-side. Capped at `maxEntries` (default 2000).
 */
export async function listDir(
  path: string,
  cwd?: string,
  maxEntries?: number,
  includeHidden?: boolean,
): Promise<DirEntry[]> {
  return invoke<DirEntry[]>("list_dir", {
    path,
    cwd: cwd ?? null,
    maxEntries: maxEntries ?? null,
    includeHidden: includeHidden ?? null,
  });
}

// ---------- projects (durable backend state) ----------

/** Load the canonical project collection from EchoAgent's private data dir. */
export async function projectsLoad<T>(): Promise<T[]> {
  return invoke<T[]>("projects_load");
}

/** Atomically replace the canonical project collection. */
export async function projectsSave(projects: unknown[]): Promise<void> {
  await invoke<void>("projects_save", { projects });
}

export interface ImportedProjectAsset {
  name: string;
  path: string;
  kind: "file" | "folder";
  ext?: string;
  sizeBytes: number;
  updatedAt: string;
}

export async function projectAssetsImport(
  projectId: string,
  sources: string[],
): Promise<ImportedProjectAsset[]> {
  return invoke<ImportedProjectAsset[]>("project_assets_import", { projectId, sources });
}

export async function projectAssetMakeDir(
  projectId: string,
  name: string,
): Promise<ImportedProjectAsset> {
  return invoke<ImportedProjectAsset>("project_asset_make_dir", { projectId, name });
}

export async function projectAssetRemove(projectId: string, path: string): Promise<void> {
  await invoke<void>("project_asset_remove", { projectId, path });
}

export async function projectAssetsRemoveAll(projectId: string): Promise<void> {
  await invoke<void>("project_assets_remove_all", { projectId });
}

export async function openLocalPath(path: string, cwd?: string): Promise<void> {
  await invoke<void>("open_path", { path, cwd: cwd ?? null });
}

/** Load a bounded, native-generated JPEG thumbnail for a local image attachment. */
export async function attachmentThumbnail(path: string): Promise<string> {
  return invoke<string>("attachment_thumbnail", { path });
}

/**
 * Persist a clipboard file blob under the app's data dir and
 * return its absolute path. The path flows straight into the existing
 * `attachments: string[]` pipeline — multimodal send, thumbnails, ACP metadata
 * — so paste and drop behave identically to a file-picker selection.
 */
export async function saveAttachmentBlob(input: {
  bytes: Uint8Array;
  mime: string;
  suggestedName?: string;
}): Promise<string> {
  // Tauri commands expect plain arrays; `Uint8Array` works because it is an
  // ArrayBufferView, but `serde` decodes `[u8]` faster from a number[].
  const byteArray = Array.from(input.bytes);
  return invoke<string>("save_attachment_blob", {
    bytes: byteArray,
    mime: input.mime,
    suggestedName: input.suggestedName ?? null,
  });
}

/** Delete an application-owned clipboard file that was removed before send. */
export async function discardAttachmentBlob(path: string): Promise<void> {
  await invoke<void>("discard_attachment_blob", { path });
}

export async function openExternalUrl(url: string): Promise<void> {
  await invoke<void>("open_url", { url });
}

export async function echoAgentDataDir(): Promise<string> {
  return invoke<string>("echo_agent_data_dir");
}

export async function openEchoAgentDataDir(): Promise<void> {
  await invoke<void>("open_echo_agent_data_dir");
}

// ---------- event subscription ----------

export interface AgentEventListeners {
  unlisten: UnlistenFn;
}

/** Subscribe to all EchoAgent events, dispatching into the provided callbacks. */
export async function subscribeAgentEvents(handlers: {
  onUpdate?: (u: SessionUpdate & { __sessionId?: string }) => void;
  onPermission?: (p: PermissionRequest) => void;
  /** Fired when a backend-side mode switch resolves an already parked permission. */
  onPermissionClosed?: (event: PermissionClosedEvent) => void;
  onComplete?: (p: PromptComplete) => void;
  /** Exact, replayable per-prompt token/cost usage from TurnCompleted. */
  onTurnUsage?: (p: TurnUsageEvent) => void;
  /** Fired when EchoAgent generates or renames a session title
   *  (`echo.agent/session_notification` → `SessionSummaryGenerated`). */
  onSummary?: (s: SessionSummaryEvent) => void;
  /** Fired on MCP connector status / init-progress notifications. */
  onMcpStatus?: (p: McpServerStatusEvent) => void;
  /** Fired when EchoAgent asks us to trust a folder (`echo.agent/folder_trust/request`). */
  onFolderTrust?: (p: FolderTrustRequest) => void;
  /** Fired when a plan is parked on the exit-plan approval ExtMethod. */
  onPlanApproval?: (p: PlanApprovalRequest) => void;
  /** Fired when plan mode is toggled (`echo.agent/toggle_plan_mode`). */
  onPlanMode?: (p: unknown) => void;
  /** Fired when the permission mode (auto/yolo) changes. */
  onPermissionMode?: (p: unknown) => void;
  /** Fired when the current repository HEAD changes. */
  onGitHead?: (p: unknown) => void;
  /** Fired when the model list updates. */
  onModelsUpdate?: (p: unknown) => void;
  /** Fired on background task lifecycle (`task_backgrounded`/`task_completed`). */
  onTaskUpdate?: (p: unknown) => void;
  /** Fired whenever a native automation run changes lifecycle state. */
  onAutomationUpdate?: (p: AutomationUpdateEvent) => void;
  /** Fired when the agent asks a question (`echo.agent/question`). */
  onQuestion?: (q: QuestionRequest) => void;
  /** Fired when a question's reverse-request closes, including timeout. */
  onQuestionClosed?: (event: QuestionClosedEvent) => void;
  /** Fired when the agent thread dies unexpectedly (panic/crash). */
  onAgentDied?: (p: { reason: string }) => void;
  /** Fired on subagent lifecycle (spawned/progress/finished). */
  onSubagent?: (e: SubagentLiveEvent) => void;
  /** Fired when a turn ends abnormally (`stopReason: "rate_limit" | "error"`).
   *  EchoAgent reports mid-stream failures via `prompt_complete` with these stop
   *  reasons rather than as a thrown error, so this event lets the UI show a
   *  friendly message instead of silently marking the turn complete. */
  onTurnError?: (e: TurnErrorEvent) => void;
}): Promise<UnlistenFn> {
  const unlisteners: UnlistenFn[] = [];
  const wire = async <T>(event: string, cb: ((p: T) => void) | undefined) => {
    if (!cb) return;
    unlisteners.push(await listen<T>(event, (e) => cb(e.payload)));
  };

  if (handlers.onUpdate) {
    unlisteners.push(
      await listen<SessionUpdate & { sessionId?: string }>("agent://update", (e) => {
        // Backend now tags each update with its sessionId. We forward it via
        // a side field so the store can filter (ignore updates for sessions
        // other than the current one — e.g. inspiration generation).
        const { sessionId, ...update } = e.payload;
        (update as SessionUpdate & { __sessionId?: string }).__sessionId = sessionId;
        handlers.onUpdate!(update as SessionUpdate & { __sessionId?: string });
      }),
    );
  }
  await wire<PermissionRequest>("agent://permission", handlers.onPermission);
  await wire<PermissionClosedEvent>("agent://permission-closed", handlers.onPermissionClosed);
  await wire<PromptComplete>("agent://complete", handlers.onComplete);
  await wire<SessionSummaryEvent>("agent://summary", handlers.onSummary);
  await wire<TurnUsageEvent>("agent://turn-usage", handlers.onTurnUsage);
  await wire<McpServerStatusEvent>("agent://mcp-status", handlers.onMcpStatus);
  await wire<FolderTrustRequest>("agent://folder-trust", handlers.onFolderTrust);
  await wire<PlanApprovalRequest>("agent://plan-approval", handlers.onPlanApproval);
  await wire("agent://plan-mode", handlers.onPlanMode);
  await wire("agent://permission-mode", handlers.onPermissionMode);
  await wire("agent://git-head", handlers.onGitHead);
  await wire("agent://models-update", handlers.onModelsUpdate);
  await wire("agent://task-update", handlers.onTaskUpdate);
  await wire<AutomationUpdateEvent>("agent://automation-update", handlers.onAutomationUpdate);
  await wire<QuestionRequest>("agent://question", handlers.onQuestion);
  await wire<QuestionClosedEvent>("agent://question-closed", handlers.onQuestionClosed);
  await wire<{ reason: string }>("agent://agent-died", handlers.onAgentDied);
  await wire<SubagentLiveEvent>("agent://subagent", handlers.onSubagent);
  await wire<TurnErrorEvent>("agent://turn-error", handlers.onTurnError);

  // Close the subscribe-after-emit race. Backend registries retain the typed
  // request until its bool-ACKed resolution, so replay is safe; stores dedupe
  // request ids when the same item was also observed live.
  try {
    const pending = await agentListPendingInteractions();
    pending.permissions.forEach((request) => handlers.onPermission?.(request));
    pending.questions.forEach((request) => handlers.onQuestion?.(request));
    pending.folderTrustRequests.forEach((request) => handlers.onFolderTrust?.(request));
    pending.planApprovals.forEach((request) => {
      handlers.onPlanApproval?.(request);
      if (handlers.onUpdate) {
        handlers.onUpdate({
          ...request,
          sessionUpdate: "plan_approval_request",
          __sessionId: request.sessionId,
        } as unknown as SessionUpdate & { __sessionId?: string });
      }
    });
  } catch (error) {
    console.warn("failed to replay pending agent interactions", error);
  }

  return () => unlisteners.forEach((u) => u());
}
