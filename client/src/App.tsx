import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { lazy, Suspense, useCallback, useEffect, useRef, useState } from "react";
import { TitleBar } from "./components/TitleBar";
import { Sidebar } from "./components/Sidebar";
import { SidebarResizeHandle } from "./components/SidebarResizeHandle";
import { SidebarLayoutContext, useAppSidebarLayout } from "./lib/sidebar-layout";
import { HomePage } from "./components/HomePage";
import { PlaceholderPage } from "./components/PlaceholderPage";
import { Toast, type ToastAction } from "./components/Toast";
// PermissionDialog is now inline in ChatView (PermissionInlineCard), not a global modal.
import { ThemeProvider } from "./components/ThemeProvider";
import type { SettingsSectionId } from "./components/SettingsPanel";
import { TasksPanel } from "./components/TasksPanel";
import { SecondarySidebar } from "./components/SecondarySidebar";
import { TopbarActions } from "./components/TopbarActions";
import { SidebarToggleIcon, EchoNewTaskIcon, ChevronLeftIcon } from "./foundation/components/Icon/icons";
import type { ModelOption } from "./components/ModelSelector";
import {
  isConfiguredModelId,
  resolveConfiguredModelId,
  resolveSessionModelId,
} from "./lib/model-selection";
import { useSessionStore } from "./stores/session-store";
import { useSessionsStore } from "./stores/sessions-store";
import { usePermissionStore } from "./stores/permission-store";
import {
  hasPendingQuestionForSession,
  useQuestionStore,
} from "./stores/question-store";
import { usePendingExpertStore } from "./stores/pending-expert-store";
import { TopbarTitle } from "./components/TopbarTitle";
import { ThumbImg } from "./components/experts-panel/shared/ThumbImg";
import {
  agentInit,
  agentNewSession,
  agentPrepareSend,
  agentSend,
  agentSendNow,
  agentCancel,
  agentLoadSession,
  agentListAllSessions,
  agentListSessions,
  agentListWorkspaces,
  agentRenameSession,
  agentDeleteSession,
  agentSetSessionArchived,
  agentSetModel,
  agentSetSessionExpert,
  agentAuthStatus,
  sessionFork,
  setCodingMode,
  togglePlanMode,
  providersList,
  flattenModels,
  filterModelsByRuntimeCatalog,
  notificationAppend,
  notificationMarkRead,
  memoryAppend,
  internalReload,
  invalidateAgentKnowledgeSourceSync,
  KNOWLEDGE_MCP_SERVER_NAME,
  subscribeAgentEvents,
  type InitResult,
  type WorkspaceInfo,
} from "./lib/agent-client";
import type { AgentEntry, SessionSummary } from "./lib/types";
import { buildCodingWorkflowPrompt } from "./features/coding/lib/workflow";
import { CodingTaskRuntimeManager } from "./features/coding/lib/coding-task-runtime";
import { hydrateProjectsFromBackend, useProjectsStore, type ProjectMeta } from "./stores/projects-store";
import {
  onSessionStatusPersistenceIssue,
  retryPendingSessionStatuses,
} from "./lib/session-status-persistence";
import {
  useMessageQueueStore,
  hasActiveItems,
  queueTerminalPolicy,
} from "./stores/message-queue-store";
import { useSubagentStore } from "./stores/subagent-store";
import {
  checkQuota,
  consumeQuotaAlert,
  isQuotaBlocking,
  recordTurnUsage,
  loadUsage,
  loadQuotaConfig,
  type QuotaConfig,
  type UsageRecord,
} from "./lib/usage-quota";
import {
  registerTelemetryProvider,
  createConsoleTelemetryProvider,
  reportEvent,
  type TelemetryProvider,
} from "./lib/telemetry-contract";
import { defaultHttpSender, exportEventsBatch, type OtlpConfig } from "./lib/otlp-exporter";
import { IS_MACOS, IS_WINDOWS } from "./lib/platform";
import { friendlyError } from "./lib/error-format";
import type { AutomationMode } from "./lib/automation-client";
import { applySessionScopedFailure } from "./lib/session-scoped-failure";
import { isGlobalShortcutBlocked } from "./lib/keyboard-scope";
import { useModalPresence } from "./lib/use-modal-focus";
import { hydrateKnowledgeSources } from "./lib/kb-source-storage";
import {
  permissionModeStatusFromEvent,
  usePermissionModeStore,
} from "./stores/permission-mode-store";
import { buildProjectPrompt, projectPromptForTurn } from "./lib/project-context";
import { migrateCatalogRootStorage } from "./lib/catalog-root-storage";
import { parseRememberArguments, type SlashCommandInvocation } from "./lib/slash-commands";
import { useUpdateStore } from "./stores/update-store";
import { useOrgSessionStore } from "./stores/org-session-store";
import { useKnowledgeStore } from "./stores/knowledge-store";
import { listenOrgSessionChanged } from "./lib/org-client";
import { indexTaskArtifacts } from "./lib/artifact-catalog";
import {
  EXPERT_PERSONA_BEGIN,
  EXPERT_PERSONA_END,
  stripInjectedUserContext,
} from "./lib/user-message";
import {
  beginAgentTurn,
  createAgentPromptId,
  isAgentPromptSettled,
} from "./lib/agent-turn";
import type { MessageRetrySendRequest } from "./lib/message-retry";
import {
  activeSubagentReturnPoint,
  popSubagentReturnPoint,
  pushSubagentReturnPoint,
  type SubagentOpenContext,
  type SubagentReturnPoint,
  type SubagentScrollRestore,
} from "./lib/subagent-navigation";
import {
  isAgentOwnedActiveStatus,
  isWaitingForUser,
  terminalSessionStatus,
} from "./lib/turn-status";
import {
  isControlledSessionStatus,
  type SessionControlAction,
} from "./lib/session-control";

const ChatView = lazy(() => import("./components/ChatView").then((module) => ({ default: module.ChatView })));
const SettingsPanel = lazy(() => import("./components/SettingsPanel").then((module) => ({ default: module.SettingsPanel })));
const SearchOverlay = lazy(() => import("./components/SearchOverlay").then((module) => ({ default: module.SearchOverlay })));
const AboutDialog = lazy(() => import("./components/AboutDialog").then((module) => ({ default: module.AboutDialog })));
const UpdateDialog = lazy(() => import("./components/UpdateDialog").then((module) => ({ default: module.UpdateDialog })));
const FolderTrustDialog = lazy(() => import("./components/FolderTrustDialog").then((module) => ({ default: module.FolderTrustDialog })));
const MeetingRecordingIndicator = lazy(() => import("./components/MeetingRecordingIndicator").then((module) => ({ default: module.MeetingRecordingIndicator })));

type ToastEntry = {
  id: number;
  message: string;
  actions: ToastAction[];
};

type SessionSelectionIntent =
  | { kind: "subagent"; source: SubagentOpenContext }
  | { kind: "return"; point: SubagentReturnPoint };

function publishQuotaAlert(
  records: UsageRecord[],
  config: QuotaConfig | null,
  sessionId: string,
): void {
  const level = consumeQuotaAlert(records, config);
  if (!level || !config) return;
  const quota = checkQuota(records, config);
  const periodLabel = config.period === "daily" ? "今日" : "本月";
  const title = level === "exceeded" ? "Token 配额已达上限" : "Token 配额接近上限";
  const body = `${periodLabel}已用 ${quota.used.toLocaleString()} / ${quota.limit.toLocaleString()} Token`
    + (config.enforcement === "block" && level === "exceeded" ? "，已暂停桌面端手动发送" : "");
  void notificationAppend("quota", title, body, sessionId, level === "exceeded" ? "error" : "warn")
    .catch(() => {});
}

/**
 * Derive a short sidebar title from the user's first message.
 * Mirrors EchoAgent's `title_fallback_from_user_text`: strip system/skill markup,
 * take the first ~10 words, cap at 40 chars.
 */
function deriveTitle(text: string): string {
  // Work from the same user-visible representation as transcript replay. In
  // particular, an expert persona is transport context, never title text.
  let clean = stripInjectedUserContext(text);
  // Strip skill XML markup (<command-name>…</command-name> etc.).
  clean = clean.replace(/<\/?command-(?:name|message|args)>/g, "").trim();
  // Take first 10 whitespace-delimited words.
  const words = clean.split(/\s+/).slice(0, 10).join(" ");
  if (!words) return "新会话";
  return words.length > 40 ? words.slice(0, 40) + "…" : words;
}

/**
 * Strip YAML frontmatter (`---\n...\n---`) from a markdown agent file and
 * return only the body (the system prompt content).
 */
function extractMarkdownBody(raw: string): string {
  const trimmed = raw.trimStart();
  if (!trimmed.startsWith("---")) return raw.trim();
  const afterOpen = trimmed.indexOf("\n");
  if (afterOpen === -1) return raw.trim();
  const rest = trimmed.slice(afterOpen + 1);
  const closeIdx = rest.search(/\n---\s*(\n|$)/);
  if (closeIdx === -1) return raw.trim();
  return rest.slice(closeIdx + 1).replace(/^\n---\s*/, "").trim();
}

/** Find a session in the complete catalog without depending on a render. */
function findSessionSummary(sessionId: string): SessionSummary | undefined {
  return useSessionsStore.getState().independent
    .find((entry) => entry.sessionId === sessionId);
}

export default function App() {
  return (
    <ThemeProvider>
      <Shell />
    </ThemeProvider>
  );
}

function Shell() {
  const notificationNavigatorRef = useRef<(sessionId: string) => Promise<void>>(async () => {});
  const notificationAutomationNavigatorRef = useRef<(automationId: string) => Promise<void>>(async () => {});
  const [init, setInit] = useState<InitResult | null>(null);
  useEffect(() => {
    if (init?.ok && "__TAURI_INTERNALS__" in window) {
      const frame = requestAnimationFrame(() => { void invoke("desktop_validation_ready").catch(console.error); });
      return () => cancelAnimationFrame(frame);
    }
  }, [init?.ok]);

  const [initError, setInitError] = useState<string | null>(null);
  const [initAttempt, setInitAttempt] = useState(0);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [settingsSection, setSettingsSection] = useState<SettingsSectionId>("model");
  const [searchOpen, setSearchOpen] = useState(false);
  const [aboutOpen, setAboutOpen] = useState(false);
  const [updateDialogOpen, setUpdateDialogOpen] = useState(false);
  const [trustRequest, setTrustRequest] = useState<{ cwd?: string; reason?: string } | null>(null);
  const [taskRefreshSignal, setTaskRefreshSignal] = useState(0);
  const [automationRefreshSignal, setAutomationRefreshSignal] = useState(0);
  const [notificationAutomationOpen, setNotificationAutomationOpen] = useState<{ id: string; sequence: number } | null>(null);
  const notificationAutomationSequenceRef = useRef(0);
  const [commandRefreshKey, setCommandRefreshKey] = useState(0);
  const [placeholderView, setPlaceholderView] = useState<string | null>(null);
  const [hasOpenedCodingWorkspace, setHasOpenedCodingWorkspace] = useState(false);
  const [createExpertRequested, setCreateExpertRequested] = useState(false);
  const [meetingLaunchModelId, setMeetingLaunchModelId] = useState<string | undefined>();
  const sidebarLayout = useAppSidebarLayout();
  const { collapsed: sidebarCollapsed, setCollapsed: setSidebarCollapsed } = sidebarLayout;
  const [toasts, setToasts] = useState<ToastEntry[]>([]);
  const [currentModelId, setCurrentModelId] = useState<string | undefined>(undefined);
  const newSessionModelOverrideRef = useRef<string | undefined>(undefined);
  const recommendedModelIdRef = useRef<string | undefined>(undefined);
  const [models, setModels] = useState<ModelOption[]>([]);
  const [modelCatalogLoading, setModelCatalogLoading] = useState(true);
  const [modelCatalogError, setModelCatalogError] = useState<string | null>(null);
  const [modelSwitching, setModelSwitching] = useState(false);
  const [loadingSession, setLoadingSession] = useState<{ sessionId: string; generation: number } | null>(null);
  const [subagentTrail, setSubagentTrail] = useState<SubagentReturnPoint[]>([]);
  const [subagentScrollRestore, setSubagentScrollRestore] = useState<SubagentScrollRestore | null>(null);
  const subagentRestoreSequenceRef = useRef(0);
  const [creatingSession, setCreatingSession] = useState(false);
  const [newTaskMode, setNewTaskMode] = useState<AutomationMode>("default");
  const [homeSendError, setHomeSendError] = useState<string | null>(null);
  const [workspaces, setWorkspaces] = useState<WorkspaceInfo[]>([]);
  /** Workspace selected for the next session; independent of the active session cwd. */
  const [newSessionTargetCwd, setNewSessionTargetCwd] = useState("");
  /** Repository owned by the dedicated Coding Workspace. */
  const [codingWorkspaceCwd, setCodingWorkspaceCwd] = useState("");
  // Dedicated Coding projects: one active project plus a persisted recent list.
  const [codingWorkspaces, setCodingWorkspaces] = useState<WorkspaceInfo[]>([]);
  const [activeCodingWorkspaceCwd, setActiveCodingWorkspaceCwd] = useState("");
  const codingLeaveGuardRef = useRef<(() => Promise<boolean>) | null>(null);
  const registerCodingLeaveGuard = useCallback((guard: (() => Promise<boolean>) | null) => {
    codingLeaveGuardRef.current = guard;
  }, []);

  // Hydrate recent Coding projects from localStorage on first mount.
  useEffect(() => {
    try {
      const raw = localStorage.getItem("echo-coding-workspaces");
      if (!raw) return;
      const parsed = JSON.parse(raw) as WorkspaceInfo[];
      if (!Array.isArray(parsed) || parsed.length === 0) return;
      setCodingWorkspaces(parsed);
      const active = localStorage.getItem("echo-coding-active-cwd");
      if (active && parsed.some((w) => w.cwd === active)) {
        setActiveCodingWorkspaceCwd(active);
      } else {
        setActiveCodingWorkspaceCwd(parsed[0].cwd);
      }
    } catch {
      /* corrupt entry, ignore */
    }
  }, []);

  // Persist recent Coding projects whenever the list changes.
  useEffect(() => {
    try {
      localStorage.setItem(
        "echo-coding-workspaces",
        JSON.stringify(codingWorkspaces),
      );
    } catch {
      /* quota or storage disabled */
    }
  }, [codingWorkspaces]);
  useEffect(() => {
    try {
      if (activeCodingWorkspaceCwd) {
        localStorage.setItem("echo-coding-active-cwd", activeCodingWorkspaceCwd);
      }
    } catch {
      /* quota */
    }
  }, [activeCodingWorkspaceCwd]);
  const [cancellingSessionId, setCancellingSessionId] = useState<string | null>(null);
  const toastTimers = useRef<Map<number, ReturnType<typeof setTimeout>>>(new Map());
  const toastIdRef = useRef(0);
  const modelsRef = useRef<ModelOption[]>([]);
  const authReadyRef = useRef(false);
  const promptedUpdateVersionRef = useRef<string | null>(null);
  const newSessionPendingRef = useRef(false);
  const selectionGenerationRef = useRef(0);
  const sessionCatalogGenerationRef = useRef(0);
  const modelCatalogGenerationRef = useRef(0);

  const dismissToast = useCallback((id: number) => {
    const timer = toastTimers.current.get(id);
    if (timer) clearTimeout(timer);
    toastTimers.current.delete(id);
    setToasts((current) => current.filter((toast) => toast.id !== id));
  }, []);

  const enqueueToast = useCallback((message: string, actions: ToastAction[] = [], durationMs = 4000) => {
    const id = ++toastIdRef.current;
    setToasts((current) => [...current, { id, message, actions }].slice(-4));
    const timer = setTimeout(() => dismissToast(id), durationMs);
    toastTimers.current.set(id, timer);
  }, [dismissToast]);

  const showToast = useCallback((message: string, durationMs = 4000) => {
    enqueueToast(message, [], durationMs);
  }, [enqueueToast]);

  useEffect(() => () => {
    for (const timer of toastTimers.current.values()) clearTimeout(timer);
    toastTimers.current.clear();
  }, []);

  const sessionStore = useSessionStore;
  const sessionsStore = useSessionsStore;
  const permissionStore = usePermissionStore;
  const questionStore = useQuestionStore;
  const updateStatus = useUpdateStore((state) => state.status);
  const availableUpdateVersion = useUpdateStore((state) => state.update?.version);
  const modalOpen = useModalPresence();

  const openSettings = useCallback((section: SettingsSectionId = "model") => {
    setSettingsSection(section);
    setSearchOpen(false);
    setAboutOpen(false);
    setSettingsOpen(true);
  }, []);
  const openSearch = useCallback(() => {
    setSettingsOpen(false);
    setAboutOpen(false);
    setSearchOpen(true);
  }, []);
  const openAbout = useCallback(() => {
    setSettingsOpen(false);
    setSearchOpen(false);
    setAboutOpen(true);
  }, []);
  const resolveTrustRequest = useCallback(() => setTrustRequest(null), []);

  useEffect(() => {
    migrateCatalogRootStorage();
    void useOrgSessionStore.getState().hydrate();
    void hydrateKnowledgeSources().catch((error) => {
      console.error("[EchoAgent] Failed to hydrate knowledge sources:", error);
      enqueueToast("知识源后端数据读取失败");
    });
    void hydrateProjectsFromBackend()
      .then(() => {
        useProjectsStore.getState().reconcileSessionArchiveStates(
          useSessionsStore.getState().independent,
        );
      })
      .catch((error) => {
        console.error("[EchoAgent] Failed to hydrate projects:", error);
        enqueueToast("项目后端数据读取失败，已使用本地缓存");
      });
  }, []);

  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | undefined;
    void listenOrgSessionChanged(({ session }) => {
      // Login, logout, account switches, and capability changes all invalidate
      // Runtime acknowledgements issued under the previous organization view.
      invalidateAgentKnowledgeSourceSync();
      useOrgSessionStore.getState().setSession(session);
    }).then((stop) => {
      if (disposed) stop();
      else unlisten = stop;
    }).catch(() => {});
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, []);

  // Organization hydration may race the native agent startup. Once the agent
  // channel is ready, re-advertise the final server-managed Skills and the
  // downloaded organization model so both catalogs observe the restored state.
  useEffect(() => {
    if (!init?.ok) return;
    void useOrgSessionStore.getState().hydrate()
      .then(() => Promise.all([internalReload("skills"), internalReload("models")]))
      .catch(() => {});
  }, [init?.ok]);

  // Update checks run after the shell is interactive and never block agent
  // initialization. Offline/VPN failures stay silent until the user opens the
  // updater manually from Help.
  useEffect(() => {
    const timer = window.setTimeout(() => {
      void useUpdateStore.getState().check(false);
    }, 3000);
    return () => window.clearTimeout(timer);
  }, []);

  useEffect(() => {
    if (updateStatus !== "available" || !availableUpdateVersion) return;
    if (promptedUpdateVersionRef.current === availableUpdateVersion) return;
    // Do not interrupt an operation that already owns focus. The effect runs
    // again when the active dialog closes, so the update prompt is deferred
    // instead of lost or stacked above an unrelated workflow.
    if (modalOpen || settingsOpen || searchOpen || aboutOpen) return;
    promptedUpdateVersionRef.current = availableUpdateVersion;
    setUpdateDialogOpen(true);
  }, [aboutOpen, availableUpdateVersion, modalOpen, searchOpen, settingsOpen, updateStatus]);

  const handleCheckForUpdates = useCallback(() => {
    setSettingsOpen(false);
    setSearchOpen(false);
    setAboutOpen(false);
    setUpdateDialogOpen(true);
    void useUpdateStore.getState().check(true);
  }, []);

  useEffect(() => {
    modelsRef.current = models;
  }, [models]);

  useEffect(() => {
    authReadyRef.current = !!init?.auth.ready;
  }, [init?.auth.ready]);

  /** Session/directory discovery is recoverable and must not take down the shell. */
  const refreshSessionCatalog = useCallback(async () => {
    const generation = ++sessionCatalogGenerationRef.current;
    const store = sessionsStore.getState();
    store.setLoading(true);
    store.setError(null);
    const [sessionResult, workspaceResult] = await Promise.allSettled([
      agentListAllSessions(true),
      agentListWorkspaces(),
    ]);
    if (sessionCatalogGenerationRef.current !== generation) return;
    const failures: string[] = [];
    if (sessionResult.status === "fulfilled") {
      sessionsStore.getState().setIndependent(sessionResult.value);
      useProjectsStore.getState().reconcileSessionArchiveStates(sessionResult.value);
    } else {
      failures.push(`会话：${friendlyError(sessionResult.reason)}`);
    }
    if (workspaceResult.status === "fulfilled") {
      sessionsStore.getState().setWorkspaces(workspaceResult.value);
      setWorkspaces(workspaceResult.value);
    } else {
      failures.push(`工作目录：${friendlyError(workspaceResult.reason)}`);
    }
    const latest = sessionsStore.getState();
    latest.setError(failures.length > 0 ? failures.join("；") : null);
    latest.setLoading(false);
  }, [sessionsStore]);

  /** Re-fetch providers + auth readiness after Settings add/edit/delete.
   *
   * Previously this only updated `models`, so the home Composer still saw
   * `apiReady=false` (from the cold-start `init.auth.ready`) and stayed
   * disabled with "请先配置 API Key" — looking like nothing changed.
   * Also, the first added model was never auto-selected as currentModelId.
   */
  const refreshModels = useCallback(async (preferredDefaultId?: string) => {
    const generation = ++modelCatalogGenerationRef.current;
    setModelCatalogLoading(true);
    setModelCatalogError(null);
    try {
      // The status check may expire an organization model lease and remove its
      // catalog entries. Read providers only after that reconciliation.
      const auth = await agentAuthStatus();
      const list = await providersList();
      if (modelCatalogGenerationRef.current !== generation) return;
      // Show only what the Runtime can actually serve. `[models]` filters
      // (allowed_models / hidden_models / disabled_models) are applied inside
      // the Runtime, so a disk entry can be absent from its catalog — offering
      // it here would let the user pick a model the backend then refuses.
      // While the catalog is still empty (init in flight) keep the disk list, or
      // the picker would blank out on every cold start.
      const options = filterModelsByRuntimeCatalog(
        flattenModels(list),
        auth.runtimeModels,
      );
      setModels(options);
      modelsRef.current = options;
      const recommendedModelId = preferredDefaultId ?? auth.defaultModelId;
      if (recommendedModelIdRef.current !== recommendedModelId) {
        newSessionModelOverrideRef.current = undefined;
        recommendedModelIdRef.current = recommendedModelId;
      }

      // Never fall back an active session to a different model in the UI. The
      // backend session keeps its persisted model until set_model succeeds.
      setCurrentModelId(() => {
        const activeId = sessionsStore.getState().currentSessionId;
        if (activeId) {
          return resolveSessionModelId(
            options,
            findSessionSummary(activeId)?.currentModelId,
          );
        }
        const explicitChoice = newSessionModelOverrideRef.current;
        if (explicitChoice && !isConfiguredModelId(options, explicitChoice)) {
          newSessionModelOverrideRef.current = undefined;
        }
        return resolveConfiguredModelId(
          options,
          newSessionModelOverrideRef.current,
          recommendedModelId,
        );
      });

      // Unlock the home Composer as soon as a configured provider exists.
      setInit((prev) => (prev ? { ...prev, auth } : prev));
      authReadyRef.current = auth.ready;
    } catch (error) {
      if (modelCatalogGenerationRef.current !== generation) return;
      // Non-fatal — the picker keeps its previous list and exposes a retry.
      setModelCatalogError(friendlyError(error));
    } finally {
      if (modelCatalogGenerationRef.current === generation) {
        setModelCatalogLoading(false);
      }
    }
  }, [sessionsStore]);

  useEffect(() => {
    let unlisten: (() => void) | null = null;
    let disposed = false;
    setInitError(null);

    // 开发环境保留最小化控制台遥测；生产环境不把事件属性写进 DevTools。
    const devMode = (import.meta as ImportMeta & { env?: { DEV?: boolean } }).env?.DEV === true;
    if (devMode) {
      registerTelemetryProvider(
        createConsoleTelemetryProvider({
          sink: (e) => console.debug(`[telemetry] ${e.level.toUpperCase()} ${e.name}`),
        }),
      );
    }
    // 若用户配置了 OTLP endpoint,额外注册 OTLP 导出 provider(自托管监控)。
    const otlpEndpoint = typeof localStorage !== "undefined" ? localStorage.getItem("echoagent.otlp.endpoint") : null;
    if (otlpEndpoint) {
      const otlpConfig: OtlpConfig = { endpoint: otlpEndpoint, serviceName: "echoagent" };
      const otlpProvider: TelemetryProvider = {
        id: "otlp",
        isEnabled: () => true,
        reportEvent: (e) => {
          void exportEventsBatch([e], otlpConfig, defaultHttpSender).then((result) => {
            if (!result.ok) console.warn(`[telemetry] OTLP export failed: HTTP ${result.status}`);
          }).catch((error) => console.warn("[telemetry] OTLP export failed:", error));
        },
        reportMetric: () => {},
      };
      registerTelemetryProvider(otlpProvider);
    }
    reportEvent("app_started", "info");

    (async () => {
      try {
        const result = await agentInit();
        // EchoAgent rejects an empty cwd, so every session needs an absolute
        // execution directory. It does not determine sidebar ownership: all
        // sessions enter the catalog and explicit project references determine
        // whether the sidebar presents one under 任务 or 项目.
        if (disposed) return;
        setNewSessionTargetCwd(result.cwd);
        sessionsStore.getState().setHomeCwd(result.cwd);
        setInit(result);
        authReadyRef.current = result.auth.ready;

        const stopListening = await subscribeAgentEvents({
          onUpdate: (u) => {
            const updateType = (u as { sessionUpdate?: string; type?: string }).sessionUpdate
              ?? (u as { type?: string }).type;
            const updateSessionId = (u as { __sessionId?: string }).__sessionId;
            const currentStatus = updateSessionId
              ? findSessionSummary(updateSessionId)?.status
              : undefined;
            if (
              updateSessionId
              && ["agent_message_chunk", "agent_thought_chunk", "tool_call", "tool_call_update"].includes(updateType ?? "")
              && (currentStatus === "pending" || isWaitingForUser(currentStatus))
            ) {
              sessionsStore.getState().upsert({
                sessionId: updateSessionId,
                status: "working",
                updatedAt: new Date().toISOString(),
              });
            } else if (
              updateSessionId
              && updateType === "plan_approval_request"
              && !sessionStore.getState().transcripts[updateSessionId]?.control
            ) {
              sessionsStore.getState().upsert({
                sessionId: updateSessionId,
                status: "awaiting_approval",
                updatedAt: new Date().toISOString(),
              });
            }
            if (updateType === "available_commands_update") {
              setCommandRefreshKey((value) => value + 1);
            }
            if (updateType === "memory_flush_completed") {
              const result = (u as { result?: string }).result;
              showToast(result?.startsWith("written")
                ? "会话摘要已保存"
                : result === "nothing to store" || result === "semantic duplicate"
                  ? "提取完成，暂无新内容"
                  : `记忆提取未完成${result ? `：${result}` : ""}`);
            } else if (updateType === "memory_dream_completed") {
              const result = (u as { result?: string }).result;
              showToast(result?.startsWith("written")
                ? "长期记忆已更新"
                : result === "nothing to consolidate"
                  ? "整理完成，暂无新内容"
                  : `记忆整理未完成${result ? `：${result}` : ""}`);
            } else if (updateType === "memory_session_saved") {
              showToast("已保存会话记忆");
            }
            sessionStore.getState().applyUpdate(u);
          },
          onPermission: (p) => {
            if (sessionStore.getState().transcripts[p.sessionId]?.control) return;
            const permissionState = permissionStore.getState();
            // A mode switch may close a backend request while its earlier
            // Tauri event is still queued for the renderer. Never resurrect it.
            if (permissionState.closedRequestIds.includes(p.requestId)) return;
            reportEvent("permission_request", "warn", { sessionId: p.sessionId });
            permissionState.request(p);
            sessionsStore.getState().upsert({
              sessionId: p.sessionId,
              status: "awaiting_permission",
              updatedAt: new Date().toISOString(),
            });
          },
          onPermissionClosed: ({ requestId, sessionId }) => {
            permissionStore.getState().close(requestId, sessionId);
            const stillPending = permissionStore.getState().queues[sessionId]?.length ?? 0;
            if (
              stillPending === 0 &&
              findSessionSummary(sessionId)?.status === "awaiting_permission"
            ) {
              sessionsStore.getState().upsert({
                sessionId,
                status: "working",
                updatedAt: new Date().toISOString(),
              });
            }
          },
          onPermissionMode: (payload) => {
            const status = permissionModeStatusFromEvent(payload);
            if (status) {
              usePermissionModeStore.getState().setStatus(status);
              if (status.sessionId) {
                sessionsStore.getState().upsert({
                  sessionId: status.sessionId,
                  permissionMode: status.permissionMode,
                });
              }
              return;
            }
            // Unscoped legacy Runtime events must never overwrite a task mode.
          },
          onGitHead: (payload) => {
            void payload;
            void refreshSessionCatalog();
          },
          onComplete: (p) => {
            const requestedAction = sessionStore.getState().transcripts[p.sessionId]?.control?.action;
            const terminalStatus = terminalSessionStatus(p, requestedAction);
            reportEvent(
              "session_complete",
              terminalStatus === "failed" ? "error" : terminalStatus === "stopped" ? "warn" : "info",
              {
                sessionId: p.sessionId,
                stopReason: p.stopReason,
                cancellationCategory: p.cancellationCategory,
              },
            );
            const summary = findSessionSummary(p.sessionId);
            const queuePolicy = queueTerminalPolicy(p.stopReason, p.cancellationCategory);
            // prompt_complete is the authoritative terminal signal and arrives
            // before PromptRequest resolves. Settle the in-flight queue row now
            // so it cannot remain visually stuck while post-turn work finishes.
            useMessageQueueStore.getState().settleSending(
              p.sessionId,
              queuePolicy.settlement,
              p.promptId || undefined,
            );
            // Completion is routed by session id in the transcript store. This
            // also finalizes a background conversation after the user switches
            // away; side-channel sessions are not added to the sidebar because
            // they have no SessionSummary entry.
            sessionStore.getState().markComplete(p);
            if (summary) {
              sessionsStore.getState().upsert({
                sessionId: p.sessionId,
                status: terminalStatus,
              });
              const transcript = sessionStore.getState().transcripts[p.sessionId];
              if (transcript) {
                indexTaskArtifacts(p.sessionId, summary.title, summary.cwd, transcript.messages);
              }

            }

            // Unknown side-channel sessions and failed turns must never start a
            // queued follow-up automatically.
            // A user cancellation/pause and every non-success terminal reason
            // intentionally stop queue progression. In particular, cancelled
            // must never make a paused conversation start the next prompt.
            if (
              !summary
              || !queuePolicy.autoAdvance
              || hasPendingQuestionForSession(p.sessionId)
            ) return;
            // Refresh the composer context-usage pill after each turn.
            // Internal/external notifications are dispatched by the Rust bridge
            // for every session (including background automation sessions).
            // 消息队列自动续发(对齐 EchoAgent message-queue):该会话若有 active
            // 队列项,取下一条继续发送,实现「回完一条自动发下一条」。
            const q = useMessageQueueStore.getState().getQueue(p.sessionId);
            if (hasActiveItems(q)) {
              const queuedModelId = resolveSessionModelId(
                modelsRef.current,
                findSessionSummary(p.sessionId)?.currentModelId,
              );
              if (!authReadyRef.current || !queuedModelId) {
                if (sessionStore.getState().sessionId === p.sessionId) {
                  sessionStore.getState().setError(
                    "⚠️ 已暂停自动续发：当前会话的模型未配置，请重新选择模型。",
                  );
                }
                sessionsStore.getState().upsert({ sessionId: p.sessionId, status: "failed" });
                return;
              }
              const queuedPromptId = createAgentPromptId();
              const next = useMessageQueueStore.getState().claimNext(
                p.sessionId,
                queuedPromptId,
              );
              if (next) {
                // PromptRequest resolves after the full model turn. Claim the
                // item before sending so the completion event cannot dispatch
                // this same item again while that promise is still pending.
                sessionsStore.getState().upsert({ sessionId: p.sessionId, status: "working" });
                sessionStore.getState().pushUser(next.text, next.attachments, p.sessionId);
                sessionStore.getState().startStreaming(p.sessionId, queuedPromptId);
                void agentSend(
                  p.sessionId,
                  next.text,
                  next.attachments,
                  next.text,
                  queuedPromptId,
                ).catch((e) => {
                  if (isAgentPromptSettled(p.sessionId, queuedPromptId)) return;
                  const detail = friendlyError(e);
                  // Preserve a rejected queued message for retry and finalize
                  // the placeholder in the transcript it actually belongs to.
                  useMessageQueueStore.getState().settleSending(
                    p.sessionId,
                    "retry",
                    queuedPromptId,
                  );
                  sessionStore.getState().markComplete({
                    sessionId: p.sessionId,
                    promptId: queuedPromptId,
                    stopReason: "error",
                    agentResult: detail,
                  });
                  if (sessionStore.getState().sessionId === p.sessionId) {
                    sessionStore.getState().setError(detail);
                  }
                  sessionsStore.getState().upsert({ sessionId: p.sessionId, status: "failed" });
                });
              }
            }
          },
          onTurnUsage: (payload) => {
            const config = loadQuotaConfig();
            const fallbackModelId = findSessionSummary(payload.sessionId)?.currentModelId;
            const next = recordTurnUsage(loadUsage(), {
              sessionId: payload.sessionId,
              promptId: payload.promptId,
              usage: payload.usage,
              occurredAt: payload.occurredAt,
              eventId: payload.eventId,
              fallbackModelId,
            }, config ?? undefined);
            publishQuotaAlert(next, config, payload.sessionId);
          },
          onSummary: ({ sessionId, title }) => {
            // EchoAgent generated (or we renamed) a session title — update the
            // sidebar entry in place. This overrides the "新会话" placeholder
            // set optimistically in handleSendNew. Stamp updatedAt so the
            // sidebar can re-sort the freshly-active session to the top.
            sessionsStore.getState().upsert({
              sessionId,
              title,
              updatedAt: new Date().toISOString(),
            });
            // Sync title into the owning project's conversation list.
            const allProjects = useProjectsStore.getState().projects;
            for (const p of allProjects) {
              if (p.conversations.some((c) => c.sessionId === sessionId)) {
                useProjectsStore.getState().updateConversationTitle(p.id, sessionId, title);
                break;
              }
            }
          },
          onFolderTrust: (p) => {
            // EchoAgent asks the user to trust a folder before running tools.
            const req = (p ?? {}) as { cwd?: string; reason?: string };
            setTrustRequest({ cwd: req.cwd, reason: req.reason });
            void notificationAppend(
              "folder_trust",
              `请求信任文件夹：${req.cwd ?? "(unknown)"}`,
              req.reason,
              undefined,
              "warn",
            );
          },
          onPlanMode: (p) => {
            // Plan mode toggled (by us or by EchoAgent). Mirror into the session store.
            const payload = (p ?? {}) as { enabled?: boolean; sessionId?: string };
            if (typeof payload.enabled === "boolean") {
              sessionStore.getState().setPlanMode(payload.enabled, payload.sessionId);
            }
          },
          onMcpStatus: (p) => {
            const isKnowledgeBridge = p.name === KNOWLEDGE_MCP_SERVER_NAME;
            const intentionalShutdown = p.reason === "config_removed" || p.reason === "disabled";
            const connectionLost = p.status === "unavailable" || p.status === "needsauth";
            if (isKnowledgeBridge && (connectionLost || intentionalShutdown || p.reason === "config_changed")) {
              const hadReadyAcknowledgement = invalidateAgentKnowledgeSourceSync(p.sessionId);
              const selected = useKnowledgeStore.getState().sessionSources[p.sessionId] ?? [];
              if (hadReadyAcknowledgement && selected.length > 0 && !intentionalShutdown) {
                const labels = [
                  selected.includes("personal") ? "个人知识" : null,
                  selected.includes("organization") ? "组织知识" : null,
                ].filter(Boolean).join("、");
                showToast(`${labels || "知识库"}连接已中断，下次发送前将自动重连`, 6000);
              }
            }
            if (connectionLost && !intentionalShutdown) {
              void notificationAppend(
                "mcp_status",
                isKnowledgeBridge ? "知识库连接已中断" : "MCP 连接器不可用",
                p.name,
                p.sessionId,
                "warn",
              );
            }
          },
          onModelsUpdate: () => {
            // EchoAgent reloaded its model catalog — keep picker + ready state in sync.
            void refreshModels();
          },
          onTaskUpdate: () => {
            // A background task changed state — bump the signal so TasksPanel refreshes.
            setTaskRefreshSignal((n) => n + 1);
          },
          onAutomationUpdate: (event) => {
            setAutomationRefreshSignal((value) => value + 1);
            if (event.sessionId && event.cwd) {
              const sessionStore = sessionsStore.getState();
              const existing = sessionStore.independent.find(
                (session) => session.sessionId === event.sessionId,
              );
              const status = event.status === "failed"
                ? "failed"
                : event.status === "success"
                  ? "completed"
                  : "working";
              sessionStore.upsert({
                sessionId: event.sessionId,
                cwd: event.cwd,
                ...(event.currentModelId && !existing?.currentModelId
                  ? { currentModelId: event.currentModelId }
                  : {}),
                // Use the automation name to hydrate a brand-new background
                // row, but never replace a title already generated for it.
                ...(existing ? {} : { title: event.automationName }),
                // A background schedule is not permission to revive a task the
                // user explicitly paused or stopped. Its own automation status
                // remains visible in AutomationPanel.
                ...(!isControlledSessionStatus(existing?.status) ? { status } : {}),
                updatedAt: new Date().toISOString(),
              });
            }
          },
          onQuestion: (q) => {
            if (sessionStore.getState().transcripts[q.sessionId]?.control) return;
            questionStore.getState().request(q);
            sessionsStore.getState().upsert({
              sessionId: q.sessionId,
              status: "awaiting_answer",
              updatedAt: new Date().toISOString(),
            });
          },
          onQuestionClosed: ({ requestId, sessionId }) => {
            questionStore.getState().dismiss(requestId, sessionId);
            const stillPending = questionStore.getState().queues[sessionId]?.length ?? 0;
            if (
              stillPending === 0
              && findSessionSummary(sessionId)?.status === "awaiting_answer"
            ) {
              sessionsStore.getState().upsert({
                sessionId,
                status: "working",
                updatedAt: new Date().toISOString(),
              });
            }
          },
          onAgentDied: ({ reason }) => {
            console.error('[EchoAgent] Agent thread died:', reason);
            const message = `AI 引擎异常退出：${reason}。请重启应用。`;
            enqueueToast(`⚠️ ${message}`, [], 8000);
            setInit((previous) => previous
              ? {
                ...previous,
                auth: {
                  ...previous.auth,
                  ready: false,
                  runtimeReady: false,
                  runtimeAuthReady: false,
                  synchronized: false,
                  runtimeModels: [],
                  lastRuntimeError: reason,
                  reason: message,
                },
              }
              : previous);
            authReadyRef.current = false;
            sessionStore.getState().failAllStreaming("error", message);
            sessionStore.getState().setError(message);
            useMessageQueueStore.getState().retryAllSending();
            permissionStore.getState().clearAll();
            questionStore.getState().clearAll();
            const failedAt = new Date().toISOString();
            for (const session of sessionsStore.getState().independent) {
              if (isAgentOwnedActiveStatus(session.status)) {
                sessionsStore.getState().upsert({
                  sessionId: session.sessionId,
                  status: "failed",
                  updatedAt: failedAt,
                });
              }
            }
            // Keep the detailed native error local: provider/runtime errors can
            // contain endpoints or filesystem paths and must not be forwarded
            // to an optional telemetry collector.
            reportEvent("agent_died", "error");
          },
          onSubagent: (e) => {
            useSubagentStore.getState().applyEvent(e);
          },
          onTurnError: (e) => {
            // EchoAgent reports mid-turn failures (429 while a tool was running,
            // connection reset, …) via prompt_complete with stopReason
            // "rate_limit"/"error". Surface a friendly message instead of
            // silently marking the turn complete.
            console.warn('[EchoAgent] Turn ended abnormally:', e);
            if (findSessionSummary(e.sessionId)) {
              sessionsStore.getState().upsert({ sessionId: e.sessionId, status: "failed" });
            }
            // Only surface for the focused session — background sessions
            // finalizing after a switch shouldn't hijack the error banner.
            const currentSessionId = sessionStore.getState().sessionId;
            if (currentSessionId && e.sessionId && e.sessionId !== currentSessionId) {
              return;
            }
            const msg =
              e.kind === "rate_limit"
                ? "⚠️ API 速率限制已触发（执行工具期间）。请等待 1-2 分钟后重试，或缩短对话上下文（新建会话）。"
                : e.detail
                  ? friendlyError(e.detail)
                  : "⚠️ 本轮执行出错，请重试。";
            sessionStore.getState().setError(msg);
            reportEvent("turn_error", "error", { sessionId: e.sessionId, kind: e.kind });
          },
        });
        if (disposed) {
          stopListening();
          return;
        }
        unlisten = stopListening;

        // These catalogs fail independently. Keep the shell usable and expose
        // local retry controls instead of converting a disk/index hiccup into a
        // fatal initialization screen.
        void refreshSessionCatalog();
        void refreshModels(result.defaultModelId);
      } catch (e) {
        if (!disposed) setInitError(friendlyError(e));
      }
    })();
    return () => {
      disposed = true;
      if (unlisten) unlisten();
    };
  }, [
    initAttempt,
    sessionStore,
    sessionsStore,
    permissionStore,
    questionStore,
    refreshModels,
    refreshSessionCatalog,
    showToast,
  ]);

  const currentSessionId = sessionsStore((s) => s.currentSessionId);
  useEffect(() => {
    void invoke("notification_set_visible_session", {
      sessionId: init?.ok && currentSessionId && !placeholderView && !settingsOpen ? currentSessionId : null,
    }).catch(() => {});
  }, [init?.ok, currentSessionId, placeholderView, settingsOpen]);
  // The active session's catalog entry drives the topbar title and cwd scoping
  // of a manual rename (mirrors EchoAgent's topbar).
  const currentEntry = sessionsStore((s) => {
    const id = s.currentSessionId;
    if (!id) return undefined;
    return s.independent.find((x) => x.sessionId === id);
  });
  const currentTitle = currentEntry?.title || "";
  const activeSessionCwd = currentEntry?.cwd;
  const streaming = sessionStore((s) => s.streaming);
  const newSessionModelId = resolveConfiguredModelId(models, currentModelId);
  const activeSessionModelId = resolveSessionModelId(models, currentEntry?.currentModelId);
  const sessionLoading = !!currentSessionId && loadingSession?.sessionId === currentSessionId;
  const modelConfigured = currentSessionId
    ? activeSessionModelId !== undefined
    : newSessionModelId !== undefined;
  const chatReady = !!init?.auth.ready && !!activeSessionModelId && !modelSwitching && !sessionLoading;
  const runtimeSetupHint = init?.auth.reason ?? "请先在「设置 → 模型」配置 API Key";
  const chatSetupHint = sessionLoading
    ? "正在加载会话信息…"
    : modelSwitching
    ? "正在切换模型…"
    : models.length === 0
      ? "请先在「设置 → 模型」配置模型"
      : !activeSessionModelId
        ? currentEntry?.currentModelId
          ? `此会话使用的模型「${currentEntry.currentModelId}」当前不可用，请在右下角选择模型`
          : "未能获取此会话的模型信息，请重新打开任务，或在右下角选择模型"
        : runtimeSetupHint;

  const showActionToast = useCallback((message: string, actions: ToastAction[], durationMs = 8000) => {
    enqueueToast(message, actions, durationMs);
  }, [enqueueToast]);

  useEffect(() => onSessionStatusPersistenceIssue(({ pendingCount }) => {
    showActionToast(
      `${pendingCount} 个任务的最新状态暂未保存，任务执行不受影响`,
      [{
        label: "重试保存",
        onClick: () => {
          const retryCount = retryPendingSessionStatuses();
          showToast(retryCount > 0 ? `正在重试保存 ${retryCount} 个任务状态` : "任务状态已保存");
        },
      }],
      12_000,
    );
  }), [showActionToast, showToast]);

  const requireConfiguredModel = (): string | undefined => {
    if (!init?.auth.ready) {
      showToast(runtimeSetupHint, 5000);
      openSettings("model");
      return undefined;
    }
    if (newSessionModelId) return newSessionModelId;
    showToast("请先在「设置 → 模型」配置模型");
    openSettings("model");
    return undefined;
  };
  const requireExplicitProjectModel = (requestedModelId: string): string | undefined => {
    if (!init?.auth.ready) {
      showToast(runtimeSetupHint, 5000);
      openSettings("model");
      return undefined;
    }
    if (isConfiguredModelId(models, requestedModelId)) return requestedModelId;
    showToast(`所选模型“${requestedModelId}”已被删除或停用，请重新选择`, 5000);
    return undefined;
  };
  const navigateNow = (label: string) => {
    setNotificationAutomationOpen(null);
    if (label !== "专家·技能·连接器") setCreateExpertRequested(false);
    if (label === "个人记忆" || label === "资料库" || label === "更多") {
      openSettings("personal-memory");
      return;
    }
    if (label === "用量统计") {
      openSettings("usage");
      return;
    }
    if (label === "通知渠道") {
      openSettings("notify-channels");
      return;
    }
    if (label === "云存储") {
      openSettings("cloud-storage");
      return;
    }
    selectionGenerationRef.current += 1;
    setPlaceholderView(label);
    if (label === "代码开发") {
      // Enter from the active task's repository when one exists. Subsequent
      // switches are kept in codingWorkspaceCwd and do not get overwritten by
      // whichever Agent session currently has global focus.
      setCodingWorkspaceCwd((current) => current || activeSessionCwd || newSessionTargetCwd);
      // Coding owns its own repository explorer, so recover horizontal space
      // while keeping the global sidebar one click away via the floating rail.
      setSidebarCollapsed(true);
      return;
    }
    sessionsStore.getState().setCurrent(null);
    sessionStore.getState().reset();
    setCurrentModelId(resolveConfiguredModelId(models, newSessionModelOverrideRef.current, init?.auth.defaultModelId));
  };
  const navigateWithGuard = (label: string, afterNavigate?: () => void) => {
    if (placeholderView === "代码开发" && label !== "代码开发"
        && label !== "用量统计" && label !== "通知渠道" && label !== "云存储"
        && codingLeaveGuardRef.current) {
      void codingLeaveGuardRef.current().then((allowed) => {
        if (allowed) {
          navigateNow(label);
          afterNavigate?.();
        }
      });
      return;
    }
    navigateNow(label);
    afterNavigate?.();
  };
  const handleNavigate = (label: string) => navigateWithGuard(label);
  const handleCreateExpert = () => {
    navigateWithGuard("专家·技能·连接器", () => setCreateExpertRequested(true));
  };
  const handlePlaceholder = (label: string) => {
    // Route a few sidebar shortcut buttons to real panels instead of toasts.
    if (label === "用户中心") {
      handleNavigate("组织");
      return;
    }
    if (label === "通知") {
      // Open the settings → 通知中心 tab where all EchoAgent
      // events are logged.
      openSettings("agent-mail");
      return;
    }
    showToast(`${label} 即将上线`);
  };

  // Sidebar project node click → open the Projects panel with that project selected.
  const handleOpenProjectFromSidebar = (projectId: string) => {
    useProjectsStore.getState().setActiveProjectId(projectId);
    handleNavigate("项目");
  };

  const ensureQuotaAllowsSend = (): boolean => {
    const config = loadQuotaConfig();
    if (!isQuotaBlocking(loadUsage(), config)) return true;
    const quota = checkQuota(loadUsage(), config!);
    showToast(`已达 ${quota.limit.toLocaleString()} Token 配额，请先在「用量统计」调整上限或策略`);
    return false;
  };

  const handleSendNew = async (text: string, attachments: string[] = []): Promise<boolean> => {
    // Composer also locks its button while this promise is pending, but this
    // app-level guard survives a view remount and protects every programmatic
    // caller from creating duplicate sessions for one submission.
    if ((!text.trim() && attachments.length === 0) || newSessionPendingRef.current) return false;
    const modelId = requireConfiguredModel();
    if (!modelId) return false;
    if (!ensureQuotaAllowsSend()) return false;
    newSessionPendingRef.current = true;
    const requestedMode = newTaskMode;
    const cwd = newSessionTargetCwd;
    let createdSessionId: string | null = null;
    setCreatingSession(true);
    setHomeSendError(null);
    try {
      const permissionState = usePermissionModeStore.getState();
      const draftPermissionMode = permissionState.capabilityStatus?.locked
        ? permissionState.capabilityStatus.permissionMode
        : permissionState.homeMode;
      const sessionId = await agentNewSession(cwd, modelId, draftPermissionMode);
      createdSessionId = sessionId;
      if (requestedMode !== "default") {
        await setCodingMode(sessionId, requestedMode);
      }
      useKnowledgeStore.getState().bindSessionSources(sessionId, true);
      setCurrentModelId(modelId);
      sessionsStore.getState().setCurrent(sessionId);
      setPlaceholderView(null);
      sessionsStore.getState().upsert({
        sessionId,
        title: deriveTitle(text),
        cwd,
        status: "working",
        currentModelId: modelId,
        permissionMode: draftPermissionMode,
      });
      usePermissionModeStore.getState().resetHomeMode();
      sessionStore.getState().setSession(sessionId);

      // Check for pending expert — inject persona invisibly.
      const pending = usePendingExpertStore.getState().expert;
      let textForAgent = text;
      if (pending && pending.prompt) {
        // Wrap persona in hidden markers. EchoAgent sees it as instructions;
        // MessageItem strips it from display on history replay.
        textForAgent = `${EXPERT_PERSONA_BEGIN}\n${pending.prompt}\n${EXPERT_PERSONA_END}\n\n${text}`;
        // Bind expert to session for persistence + UI badge.
        agentSetSessionExpert(sessionId, pending.expertId, pending.name, pending.source, pending.avatarLocal)
          .catch(() => {});
        sessionsStore.getState().upsert({ sessionId, expertId: pending.expertId, expertName: pending.name, expertAvatar: pending.avatarLocal });
        usePendingExpertStore.getState().clear();
      }

      const accepted = beginAgentTurn({
        sessionId,
        promptText: textForAgent,
        displayText: text,
        attachments,
      });
      if (!accepted) return false;
      setNewTaskMode("default");
      setHomeSendError(null);
      return true;
    } catch (e) {
      console.error('[EchoAgent] handleSendNew error:', e);
      if (createdSessionId && sessionsStore.getState().currentSessionId !== createdSessionId) {
        try {
          await agentDeleteSession(createdSessionId, cwd);
        } catch (cleanupError) {
          console.warn("[EchoAgent] failed to clean up an unstarted automation session", cleanupError);
        }
      }
      const error = friendlyError(e);
      setHomeSendError(error);
      showToast(`创建会话失败：${error}`, 6000);
      return false;
    } finally {
      newSessionPendingRef.current = false;
      setCreatingSession(false);
    }
  };

  const handleSendCurrent = async (
    text: string,
    attachments: string[] = [],
    queueItemId?: string,
    promptTextOverride?: string,
  ): Promise<boolean> => {
    if (!text.trim() && attachments.length === 0) return false;
    if (!currentSessionId) return handleSendNew(text, attachments);
    const sessionId = currentSessionId;
    if (hasPendingQuestionForSession(sessionId)) {
      showToast("请先回答当前问题");
      return false;
    }
    // Guard against double-send / send-during-streaming. Composer also guards
    // via its `streaming` prop, but that value can be stale within the same
    // render tick; the store flag is the source of truth. A second pushUser +
    // startStreaming would orphan an empty placeholder that never completes.
    if (sessionStore.getState().streaming) return false;
    if (modelSwitching || sessionLoading) {
      showToast(sessionLoading ? "正在加载会话，请稍候" : "正在切换模型，请稍候");
      return false;
    }
    if (!init?.auth.ready) {
      showToast(runtimeSetupHint, 5000);
      openSettings("model");
      return false;
    }
    if (!activeSessionModelId) {
      showToast("当前会话的模型未配置，请先在输入框右下角重新选择模型");
      return false;
    }
    if (!ensureQuotaAllowsSend()) return false;
    const queuePromptId = queueItemId ? createAgentPromptId() : undefined;
    let sendText = text;
    let sendAttachments = attachments;
    if (queueItemId && queuePromptId) {
      const claimed = useMessageQueueStore.getState().claimById(
        sessionId,
        queueItemId,
        queuePromptId,
      );
      if (!claimed) return false;
      sendText = claimed.text;
      sendAttachments = claimed.attachments ?? [];
    }
    try {
      // A conversation created from the project node may intentionally be
      // empty. Bind the project contract to its first real user turn so the
      // session does not silently behave like an ordinary workspace chat.
      const project = useProjectsStore.getState().projects.find((item) =>
        item.conversations.some((conversation) => conversation.sessionId === sessionId),
      );
      const projectConversation = project?.conversations.find((conversation) => conversation.sessionId === sessionId);
      const isFirstUserTurn = !sessionStore.getState().messages.some(
        (message) => message.role === "user",
      );
      const promptBody = promptTextOverride ?? sendText;
      const { promptText: textForAgent, consumedPendingContext } = projectPromptForTurn(
        project, projectConversation, isFirstUserTurn, promptBody,
      );
      const accepted = beginAgentTurn({
        sessionId,
        promptText: textForAgent,
        displayText: sendText,
        attachments: sendAttachments,
        promptId: queuePromptId,
        onRejected: consumedPendingContext || queuePromptId
          ? () => {
              if (consumedPendingContext && project) {
                useProjectsStore.getState().setProjectContextPending(project.id, sessionId, true);
              }
              if (!queuePromptId) return;
              useMessageQueueStore.getState().settleSending(
                sessionId,
                "retry",
                queuePromptId,
              );
            }
          : undefined,
      });
      if (!accepted) {
        if (queuePromptId) {
          useMessageQueueStore.getState().settleSending(
            sessionId,
            "retry",
            queuePromptId,
          );
        }
        showToast("当前会话已切换，请重新发送");
        return false;
      }
      if (consumedPendingContext && project) {
        useProjectsStore.getState().setProjectContextPending(project.id, sessionId, false);
      }
      return true;
    } catch (e) {
      if (queuePromptId) {
        useMessageQueueStore.getState().settleSending(
          sessionId,
          "retry",
          queuePromptId,
        );
      }
      sessionStore.getState().rollbackPendingTurn();
      sessionStore.getState().setError(friendlyError(e));
      sessionsStore.getState().upsert({ sessionId, status: "failed" });
      return false;
    }
  };

  const handlePrepareRetry = async (sessionId: string): Promise<boolean> => {
    if (sessionStore.getState().transcripts[sessionId]?.streamingMessageId) return false;
    if (modelSwitching || sessionLoading) {
      showToast(sessionLoading ? "正在加载会话，请稍候" : "正在切换模型，请稍候");
      return false;
    }
    if (!init?.auth.ready) {
      showToast(runtimeSetupHint, 5000);
      openSettings("model");
      return false;
    }
    const summary = findSessionSummary(sessionId);
    if (!summary?.currentModelId && sessionId !== currentSessionId) {
      showToast("原会话的模型不可用，请返回该会话重新选择模型");
      return false;
    }
    if (sessionId === currentSessionId && !activeSessionModelId) {
      showToast("当前会话的模型未配置，请先在输入框右下角重新选择模型");
      return false;
    }
    if (!ensureQuotaAllowsSend()) return false;
    try {
      await agentPrepareSend(sessionId);
      return true;
    } catch (error) {
      showToast(`暂时无法重试：${friendlyError(error)}`, 6000);
      return false;
    }
  };

  const handleRetrySend = ({
    sessionId,
    displayText,
    promptText,
    attachments,
  }: MessageRetrySendRequest): boolean => {
    const accepted = beginAgentTurn({
      sessionId,
      promptText,
      displayText,
      attachments,
      allowBackgroundSession: true,
    });
    if (!accepted) showToast("原会话状态已变化，请返回该会话重试");
    return accepted;
  };

  /**
   * Open an Agent session for a coding-workbench task.
   *
   * The workbench owns the task itself (requirement, phase, change set); this
   * only creates the session it runs in, so session lifecycle stays with the
   * rest of the application. Echo Code always runs as an Agent; permission
   * policy independently defines approval autonomy.
   */
  const handleStartCodingRun = async (
    root: string,
    requirement: string,
    requestedModelId: string | undefined,
    contextPaths: string[] = [],
    onSessionReady?: (sessionId: string) => Promise<void>,
    promptTextOverride?: string,
  ): Promise<string | undefined> => {
    setCodingWorkspaceCwd(root);
    // Register the task's repository in recent Coding projects and activate it.
    if (root) {
      setCodingWorkspaces((current) =>
        current.some((w) => w.cwd === root)
          ? current
          : [{ cwd: root, sessionCount: 0 }, ...current],
      );
      setActiveCodingWorkspaceCwd(root);
    }
    const modelId = isConfiguredModelId(models, requestedModelId)
      ? requestedModelId
      : requireConfiguredModel();
    if (!modelId || !ensureQuotaAllowsSend()) return undefined;
    const permissionState = usePermissionModeStore.getState();
    const permissionMode = permissionState.capabilityStatus?.locked
      ? permissionState.capabilityStatus.permissionMode
      : permissionState.homeMode;
    let sessionId: string | undefined;
    try {
      sessionId = await agentNewSession(root, modelId, permissionMode);
      useKnowledgeStore.getState().bindSessionSources(sessionId, true);
      setCurrentModelId(modelId);
      sessionsStore.getState().setCurrent(sessionId);
      sessionsStore.getState().upsert({
        sessionId,
        title: `代码开发：${deriveTitle(requirement)}`,
        cwd: root,
        status: "working",
        currentModelId: modelId,
        permissionMode,
      });
      sessionStore.getState().setSession(sessionId);
      setPlaceholderView("代码开发");
      // Persist the task/session ownership before the first Agent token can be
      // produced. Otherwise a very fast turn can finish before the workbench
      // knows which task should receive its Git sync event.
      if (onSessionReady) await onSessionReady(sessionId);
      // The mode is an internal runtime safety primitive, not a user-facing
      // product choice. Every Echo Code task starts with write-capable Agent.
      await setCodingMode(sessionId, "agent");
      const promptText = promptTextOverride ?? buildCodingWorkflowPrompt(requirement, contextPaths);
      const accepted = beginAgentTurn({
        sessionId,
        promptText,
        displayText: requirement,
      });
      if (!accepted) throw new Error("代码开发会话未能获得前台焦点");
      usePermissionModeStore.getState().resetHomeMode();
      return sessionId;
    } catch (error) {
      if (sessionId) {
        sessionsStore.getState().upsert({ sessionId, status: "failed" });
      }
      showToast(`启动代码开发失败：${friendlyError(error)}`, 6000);
      return undefined;
    }
  };

  /**
   * Replace the active turn without making the user press Stop first. Runtime
   * owns this as one atomic sendNow operation, so the cancelled completion of
   * the old turn cannot race a separate follow-up send.
   */
  const handleSendNowCurrent = (
    text: string,
    attachments: string[] = [],
    queueItemId?: string,
  ): boolean => {
    if ((!text.trim() && attachments.length === 0) || !currentSessionId) return false;
    const sessionId = currentSessionId;
    if (hasPendingQuestionForSession(sessionId)) {
      showToast("请先回答当前问题");
      return false;
    }
    const transcript = sessionStore.getState();
    if (!transcript.streaming || transcript.sendNowPending) return false;
    if (modelSwitching || sessionLoading) {
      showToast(sessionLoading ? "正在加载会话，请稍候" : "正在切换模型，请稍候");
      return false;
    }
    if (!init?.auth.ready) {
      showToast(runtimeSetupHint, 5000);
      openSettings("model");
      return false;
    }
    if (!activeSessionModelId) {
      showToast("当前会话的模型未配置，请先在输入框右下角重新选择模型");
      return false;
    }
    if (!ensureQuotaAllowsSend()) return false;

    const promptId = createAgentPromptId();
    let sendText = text;
    let sendAttachments = attachments;
    if (queueItemId) {
      const claimed = useMessageQueueStore.getState().claimById(
        sessionId,
        queueItemId,
        promptId,
      );
      if (!claimed) return false;
      sendText = claimed.text;
      sendAttachments = claimed.attachments ?? [];
    }
    transcript.requestSendNow(promptId, sessionId);
    sessionsStore.getState().upsert({ sessionId, status: "working" });

    // PromptRequest resolves only after the replacement turn ends. Detach it
    // so the composer can clear immediately, just like a normal admitted turn.
    void agentSendNow(
      sessionId,
      sendText,
      sendAttachments,
      sendText,
      promptId,
    ).catch((error) => {
      if (isAgentPromptSettled(sessionId, promptId)) return;
      const detail = friendlyError(error);
      const latest = sessionStore.getState();
      const targetTranscript = latest.transcripts[sessionId];
      const promptWasAdmitted = targetTranscript?.messages.some(
        (message) => message.role === "assistant" && message.promptId === promptId,
      ) ?? false;
      latest.rejectSendNow(promptId, sessionId);
      latest.markComplete({
        sessionId,
        promptId,
        stopReason: "error",
        agentResult: detail,
      });
      if (queueItemId) {
        // Restore the same row in place: its id and relative order are stable.
        useMessageQueueStore.getState().settleSending(sessionId, "retry", promptId);
      } else if (!promptWasAdmitted) {
        // A direct composer send that never reached Runtime still needs a safe
        // retry path. Once admitted, duplicating it would repeat side effects.
        useMessageQueueStore.getState().enqueue(sessionId, sendText, sendAttachments);
      }
      if (latest.sessionId === sessionId) latest.setError(detail);
      const stillWorking = useSessionStore.getState().transcripts[sessionId];
      sessionsStore.getState().upsert({
        sessionId,
        status:
          stillWorking?.streamingMessageId || stillWorking?.pendingSendNowPromptId
            ? "working"
            : "failed",
      });
      const preservation = queueItemId || !promptWasAdmitted
        ? "，内容已保留在待发送队列"
        : "";
      showToast(`立即发送失败${preservation}：${detail}`, 6000);
    });
    return true;
  };

  const handleCancel = async (
    action: SessionControlAction = "stop",
  ): Promise<boolean> => {
    if (!currentSessionId || cancellingSessionId) return false;
    const sessionId = currentSessionId;
    const beforeCancel = sessionStore.getState().transcripts[sessionId];
    const activeMessage = beforeCancel?.messages.find(
      (message) => message.id === beforeCancel.streamingMessageId,
    );
    const activePromptId = beforeCancel?.pendingSendNowPromptId ?? activeMessage?.promptId;
    if (!beforeCancel?.streamingMessageId && !beforeCancel?.pendingSendNowPromptId) {
      showToast(action === "pause" ? "当前任务没有正在运行的内容" : "当前任务已停止");
      return false;
    }
    sessionStore.getState().requestControl(sessionId, action, activePromptId);
    sessionsStore.getState().upsert({
      sessionId,
      status: action === "pause" ? "pausing" : "stopping",
      updatedAt: new Date().toISOString(),
    });
    setCancellingSessionId(sessionId);
    try {
      await agentCancel(sessionId, action, activePromptId);
      // Native resolves only after the session actor has applied cancellation.
      // prompt_complete may reach the renderer just before or just after this
      // acknowledgement; both paths are idempotent and represent real stopped
      // work, so the UI can settle immediately without a speculative timer.
      sessionStore.getState().confirmControl(sessionId, action);
      sessionsStore.getState().upsert({
        sessionId,
        status: action === "pause" ? "paused" : "stopped",
        updatedAt: new Date().toISOString(),
      });
      const permissionState = permissionStore.getState();
      for (const request of permissionState.queues[sessionId] ?? []) {
        permissionState.close(request.requestId, sessionId);
      }
      const questionState = questionStore.getState();
      for (const request of questionState.queues[sessionId] ?? []) {
        questionState.dismiss(request.requestId, sessionId);
      }
      useMessageQueueStore.getState().settleSending(
        sessionId,
        "consume",
        activePromptId,
      );
      return true;
    } catch (e) {
      sessionStore.getState().rejectControl(sessionId, action);
      const transcript = sessionStore.getState().transcripts[sessionId];
      sessionsStore.getState().upsert({
        sessionId,
        status: transcript?.streamingMessageId || transcript?.pendingSendNowPromptId
          ? "working"
          : "failed",
      });
      if (sessionStore.getState().sessionId === sessionId) {
        sessionStore.getState().setError(friendlyError(e));
      }
      showToast(`${action === "pause" ? "暂停" : "停止"}失败：${friendlyError(e)}`, 5000);
      return false;
    } finally {
      setCancellingSessionId((pending) => pending === sessionId ? null : pending);
    }
  };

  // Topbar title rename — EchoAgent's `echo.agent/session/rename`. EchoAgent broadcasts
  // SessionSummaryGenerated on success (agent://summary → onSummary upserts the
  // same entry); we also upsert optimistically to avoid a flicker while the
  // event round-trips. On failure we rethrow so TopbarTitle reverts its draft.
  const handleRenameTitle = async (newTitle: string) => {
    if (!currentEntry) return;
    try {
      await agentRenameSession(currentEntry.sessionId, newTitle, currentEntry.cwd);
      sessionsStore.getState().upsert({
        sessionId: currentEntry.sessionId,
        title: newTitle,
      });
    } catch (e) {
      showToast(`重命名失败：${String(e).replace(/^Error:\s*/, "")}`);
      throw e;
    }
  };

  // Model picker: switch the current session's model via EchoAgent's set_model.
  // If there's no session yet, we just remember the choice and apply it in
  // handleSendNew when the session is created.
  const handleModelChange = async (modelId: string) => {
    if (!isConfiguredModelId(models, modelId)) {
      showToast("该模型已不在配置列表中");
      return;
    }
    if (!currentSessionId) {
      newSessionModelOverrideRef.current = modelId;
      setCurrentModelId(modelId);
      return;
    }
    if (modelSwitching || sessionLoading) return;

    const sessionId = currentSessionId;
    const commit = () => {
      sessionsStore.getState().upsert({
        sessionId,
        currentModelId: modelId,
      });
      if (sessionsStore.getState().currentSessionId === sessionId) {
        setCurrentModelId(modelId);
        sessionStore.getState().setError(null);
      }
    };
    setModelSwitching(true);
    // EchoAgent only knows about sessions it has *loaded* into memory. A session
    // picked from the sidebar (agent_list_sessions) isn't loaded until
    // agentLoadSession runs, and after an agent restart even a freshly-used
    // session can be gone. set_session_model then fails with
    // "unknown session id". Recover transparently: load the session into the
    // agent (replaying its history) and retry the switch once.
    const trySet = () => agentSetModel(sessionId, modelId);
    try {
      await trySet();
      commit();
    } catch (e) {
      const msg = String(e);
      // Incompatible harness is a hard error — loading won't help.
      if (/incompatible|start_new_session/i.test(msg)) {
        showToast("该会话无法切换到此模型，请新建会话");
        return;
      }
      // Session genuinely unknown to EchoAgent — load it (with its own cwd) then
      // retry. currentEntry carries the cwd the session belongs to.
      if (/unknown session/i.test(msg)) {
        try {
          const sessionCwd = findSessionSummary(sessionId)?.cwd;
          if (!sessionCwd) {
            showToast("模型切换失败：无法确定当前会话的工作区");
            return;
          }
          await agentLoadSession(sessionId, sessionCwd);
          if (sessionsStore.getState().currentSessionId === sessionId) {
            setTaskRefreshSignal((value) => value + 1);
          }
          await trySet();
          commit();
          return;
        } catch (e2) {
          showToast(`模型切换失败：${String(e2).replace(/^Error:\s*/, "")}`);
          return;
        }
      }
      showToast(`模型切换失败：${msg.replace(/^Error:\s*/, "")}`);
    } finally {
      setModelSwitching(false);
    }
  };

  // Working-directory picker: only re-aim the cwd for the NEXT new session.
  // Existing task/project ownership and the current transcript stay intact.
  // No agent re-init is needed: spawn_agent_runtime ignores its cwd and every session
  // carries its own cwd at new_session/load_session time.
  const handleSelectWorkspace = (newCwd: string) => {
    setNewSessionTargetCwd(newCwd);
    if (!newCwd) return;
    const current = sessionsStore.getState().workspaces;
    if (current.some((workspace) => workspace.cwd === newCwd)) return;
    const next = [{ cwd: newCwd, sessionCount: 0 }, ...current];
    sessionsStore.getState().setWorkspaces(next);
    setWorkspaces(next);
  };

  const handleSelectCodingWorkspace = (newCwd: string) => {
    setCodingWorkspaceCwd(newCwd);
    // Register the cwd in recent Coding projects and activate it.
    if (newCwd) {
      setCodingWorkspaces((current) =>
        current.some((w) => w.cwd === newCwd)
          ? current
          : [{ cwd: newCwd, sessionCount: 0 }, ...current],
      );
      setActiveCodingWorkspaceCwd(newCwd);
    }
    if (!newCwd) return;
    // Keep the IDE repository independent from the ordinary chat composer's
    // "next session" cwd. It still belongs in recent workspaces, but leaving
    // Coding must not silently retarget an unrelated new chat.
    const current = sessionsStore.getState().workspaces;
    if (current.some((workspace) => workspace.cwd === newCwd)) return;
    const next = [{ cwd: newCwd, sessionCount: 0 }, ...current];
    sessionsStore.getState().setWorkspaces(next);
    setWorkspaces(next);
  };

  const handleCloseCodingWorkspace = (cwd: string) => {
    setCodingWorkspaces((current) => {
      const next = current.filter((w) => w.cwd !== cwd);
      if (cwd === activeCodingWorkspaceCwd) {
        setActiveCodingWorkspaceCwd(next[0]?.cwd ?? "");
      }
      if (cwd === codingWorkspaceCwd) {
        setCodingWorkspaceCwd(next[0]?.cwd ?? "");
      }
      return next;
    });
  };

  const createNewSessionView = () => {
    selectionGenerationRef.current += 1;
    setPlaceholderView(null);
    sessionsStore.getState().setCurrent(null);
    sessionStore.getState().reset();
    usePermissionModeStore.getState().resetHomeMode();
    setNewTaskMode("default");
    setCurrentModelId(resolveConfiguredModelId(models, newSessionModelOverrideRef.current, init?.auth.defaultModelId));
  };
  const handleNewSession = () => {
    if (placeholderView === "代码开发" && codingLeaveGuardRef.current) {
      void codingLeaveGuardRef.current().then((allowed) => {
        if (allowed) createNewSessionView();
      });
      return;
    }
    createNewSessionView();
  };

  const handleStartOrganizationConversation = () => {
    const knowledge = useKnowledgeStore.getState();
    knowledge.setDefaultSources(["organization"]);
    knowledge.setDefaultOrganizationScopeIds([]);
    handleNewSession();
  };

  /** Navigate to home page without resetting session state (used after expert summon). */
  const handleGoHome = () => {
    if (placeholderView === "代码开发" && codingLeaveGuardRef.current) {
      void codingLeaveGuardRef.current().then((allowed) => {
        if (allowed) {
          selectionGenerationRef.current += 1;
          setPlaceholderView(null);
          sessionsStore.getState().setCurrent(null);
          setCurrentModelId(resolveConfiguredModelId(models, newSessionModelOverrideRef.current, init?.auth.defaultModelId));
        }
      });
      return;
    }
    selectionGenerationRef.current += 1;
    setPlaceholderView(null);
    sessionsStore.getState().setCurrent(null);
    setCurrentModelId(resolveConfiguredModelId(models, newSessionModelOverrideRef.current, init?.auth.defaultModelId));
  };

  const leaveSessionIfCurrent = (sessionId: string) => {
    if (sessionsStore.getState().currentSessionId !== sessionId) return;
    selectionGenerationRef.current += 1;
    sessionsStore.getState().setCurrent(null);
    sessionStore.getState().reset();
    setPlaceholderView(null);
    setCurrentModelId(resolveConfiguredModelId(models, newSessionModelOverrideRef.current, init?.auth.defaultModelId));
  };

  const clearDeletedSessionState = (sessionId: string) => {
    useProjectsStore.getState().removeSessionReferences(sessionId);
    usePermissionModeStore.getState().clearSession(sessionId);
    usePermissionStore.getState().clearSession(sessionId);
    useQuestionStore.getState().clearSession(sessionId);
    useMessageQueueStore.getState().clear(sessionId);
    useSubagentStore.getState().clearSession(sessionId);
    leaveSessionIfCurrent(sessionId);
    // Permanent deletion must also forget any parked, in-memory transcript.
    sessionStore.getState().dropSessionCache(sessionId);
  };

  const syncArchivedSessionState = (sessionId: string, archived: boolean) => {
    sessionsStore.getState().upsert({ sessionId, archived });
    useProjectsStore.getState().setSessionArchived(sessionId, archived);
    if (archived) leaveSessionIfCurrent(sessionId);
  };

  const showArchivedSessionActions = (sessionId: string) => {
    showActionToast("已归档，会话已从侧边栏收起", [
      {
        label: "撤销",
        onClick: async () => {
          try {
            const restored = await agentSetSessionArchived(sessionId, false);
            syncArchivedSessionState(sessionId, restored);
            showToast("已恢复会话");
          } catch (cause) {
            showToast(`恢复失败：${String(cause).replace(/^Error:\s*/, "")}`, 5000);
          }
        },
      },
      { label: "管理归档", onClick: () => openSettings("archived") },
    ]);
  };

  /** One lifecycle implementation is shared by the task list and every project surface. */
  const handleRenameSession = async (sessionId: string, title: string, explicitCwd?: string) => {
    const summary = findSessionSummary(sessionId);
    const cwd = summary?.cwd || explicitCwd;
    await agentRenameSession(sessionId, title, cwd);
    sessionsStore.getState().upsert({ sessionId, title });
    const projectStore = useProjectsStore.getState();
    for (const project of projectStore.projects) {
      if (project.conversations.some((conversation) => conversation.sessionId === sessionId)) {
        projectStore.updateConversationTitle(project.id, sessionId, title);
      }
    }
    showToast("已重命名对话");
  };

  const handleArchiveSession = async (sessionId: string, archived: boolean, _explicitCwd?: string) => {
    const next = await agentSetSessionArchived(sessionId, archived);
    syncArchivedSessionState(sessionId, next);
    if (next) showArchivedSessionActions(sessionId);
    else showToast("已恢复会话");
  };

  const handleDeleteSession = async (sessionId: string, explicitCwd?: string) => {
    const summary = findSessionSummary(sessionId);
    const cwd = summary?.cwd || explicitCwd;
    const result = await agentDeleteSession(sessionId, cwd);
    // Leave while the catalog still identifies this as the current session;
    // `remove` also clears currentSessionId and would otherwise strand its transcript.
    clearDeletedSessionState(sessionId);
    sessionsStore.getState().remove(sessionId, cwd);
    if (result?.memoryCleanupWarning) {
      showActionToast(`对话已删除，但自动摘要清理失败：${result.memoryCleanupWarning}`, [
        { label: "检查会话摘要", onClick: () => openSettings("personal-memory") },
      ]);
    } else {
      showActionToast("对话与自动摘要已删除。已整理的长期记忆可能仍包含相关内容。", [
        { label: "检查长期记忆", onClick: () => openSettings("personal-memory") },
      ]);
    }
  };

  // Application-level shortcuts shown in Settings. Composer-specific Enter,
  // Shift+Enter, slash and @ behavior stays scoped to the input component.
  useEffect(() => {
    const onShortcut = (event: KeyboardEvent) => {
      if (!(event.metaKey || event.ctrlKey) || event.altKey || event.repeat) return;
      // A modal owns the keyboard while it is open. This also covers dialogs
      // mounted inside ChatView (for example message feedback), not just Shell
      // state, and prevents shortcuts from mutating content behind an overlay.
      if (isGlobalShortcutBlocked()) return;
      const key = event.key.toLowerCase();
      if (key === "n") {
        event.preventDefault();
        setSettingsOpen(false);
        setSearchOpen(false);
        handleNewSession();
      } else if (key === "k") {
        event.preventDefault();
        openSearch();
      } else if (event.key === ",") {
        event.preventDefault();
        openSettings("model");
      } else if (key === "b") {
        event.preventDefault();
        setSidebarCollapsed((value) => !value);
      }
    };
    window.addEventListener("keydown", onShortcut);
    return () => window.removeEventListener("keydown", onShortcut);
  }, [models, openSearch, openSettings]);

  const handleSelectSession = async (
    sessionId: string,
    sessionCwd?: string,
    preservePlaceholder = false,
    strict = false,
    intent?: SessionSelectionIntent,
  ) => {
    const focusedSessionId = sessionsStore.getState().currentSessionId;
    if (intent?.kind === "subagent" && focusedSessionId !== intent.source.parentSessionId) return;
    if (intent?.kind === "return" && focusedSessionId !== intent.point.childSessionId) return;
    if (!preservePlaceholder && placeholderView === "代码开发" && codingLeaveGuardRef.current) {
      if (!(await codingLeaveGuardRef.current())) {
        if (strict) throw new Error("请先处理代码开发中的未保存内容");
        return;
      }
    }
    const generation = ++selectionGenerationRef.current;
    let entry = findSessionSummary(sessionId);
    // Subagent child sessions can be created after the last catalog refresh
    // and may live in an isolated worktree. Refresh the global catalog first;
    // a parent cwd is not authoritative for those children.
    if (!entry) {
      try {
        const list = await agentListAllSessions(true);
        if (selectionGenerationRef.current !== generation) {
          if (strict) throw new Error("任务打开已被新的导航操作取代");
          return;
        }
        sessionsStore.getState().mergeSessions(list);
        entry = list.find((item) => item.sessionId === sessionId);
      } catch {
        // Fall through to the scoped compatibility lookup below. Older
        // runtimes may not support global discovery.
      }
      // FTS/project/automation links can still provide a precise cwd. Hydrate
      // it as a compatibility fallback without replacing other directories.
      if (!entry && sessionCwd) {
        try {
          const list = await agentListSessions(sessionCwd, true);
          if (selectionGenerationRef.current !== generation) {
            if (strict) throw new Error("任务打开已被新的导航操作取代");
            return;
          }
          sessionsStore.getState().mergeSessions(list);
          entry = list.find((item) => item.sessionId === sessionId);
        } catch (error) {
          if (selectionGenerationRef.current === generation) {
            showToast(`加载会话信息失败：${friendlyError(error)}`, 6000);
          }
          if (strict) throw error;
          return;
        }
      }
    }
    if (selectionGenerationRef.current !== generation) {
      if (strict) throw new Error("任务打开已被新的导航操作取代");
      return;
    }
    if (!entry) {
      if (strict) throw new Error("关联任务不存在、已归档或当前无权访问");
      showToast("无法打开会话：会话不存在、已归档或当前无权访问", 5000);
      return;
    }
    if (entry.archived) {
      if (strict) throw new Error("关联任务已归档，请先在归档管理中恢复");
      showActionToast("该会话已归档，请先恢复后继续", [
        { label: "管理归档", onClick: () => openSettings("archived") },
      ], 6000);
      return;
    }
    setLoadingSession({ sessionId, generation });
    const persistedModelId = entry.currentModelId;
    const selectedModelId = resolveSessionModelId(models, persistedModelId);
    if (!preservePlaceholder) setPlaceholderView(null);
    if (intent?.kind === "subagent") {
      const point = { ...intent.source, childSessionId: sessionId };
      setSubagentTrail((trail) => pushSubagentReturnPoint(trail, point));
      setSubagentScrollRestore(null);
    } else if (intent?.kind === "return") {
      setSubagentTrail((trail) => popSubagentReturnPoint(trail, intent.point));
      setSubagentScrollRestore({ ...intent.point, sequence: ++subagentRestoreSequenceRef.current });
    } else {
      // An unrelated task selection is not a move up the subagent hierarchy.
      setSubagentTrail([]);
      setSubagentScrollRestore(null);
    }
    sessionsStore.getState().setCurrent(sessionId);
    // Reflect the model actually persisted by this session. Do not fall back to
    // the first configured model: that would only change the picker, not the
    // backend session, and could route the next prompt to stale runtime settings.
    setCurrentModelId(selectedModelId);
    // The runtime replay is the authority for lifecycle history. Clear the
    // in-memory projection so rewind/deletion and stale cached sessions cannot
    // leave ghost subagents; replay immediately reconstructs the durable list.
    useSubagentStore.getState().clearSession(sessionId);
    // setSession no longer wipes the transcript — it just moves focus. If we
    // already have a cached transcript for this session it arms replay
    // suppression so EchoAgent's history re-stream can't duplicate/merge it; if we
    // don't (first open / post-restart) the upcoming replay fills the empty
    // transcript. Either way the focused mirror is refreshed in one step.
    sessionStore.getState().setSession(sessionId);
    let historyLoaded = false;
    try {
      // Load with the session's own cwd. Opening history must not re-aim the
      // working directory selected for the next new task.
      const loadedModelId = await agentLoadSession(sessionId, entry.cwd);
      historyLoaded = true;
      if (selectionGenerationRef.current !== generation) {
        if (strict) throw new Error("任务打开已被新的导航操作取代");
        return;
      }
      // The load response is authoritative; older runtimes may omit models.
      const actualModelId = loadedModelId || findSessionSummary(sessionId)?.currentModelId;
      sessionsStore.getState().upsert({ sessionId, currentModelId: actualModelId });
      if (sessionsStore.getState().currentSessionId === sessionId) {
        setCurrentModelId(resolveSessionModelId(modelsRef.current, actualModelId));
        setTaskRefreshSignal((value) => value + 1);
      }
      const transcript = sessionStore.getState().transcripts[sessionId];
      if (transcript) {
        indexTaskArtifacts(
          sessionId,
          entry.title ?? "未命名任务",
          entry.cwd,
          transcript.messages,
        );
      }
      // Populate the context-usage pill for the freshly loaded session.
    } catch (e) {
      if (selectionGenerationRef.current === generation && sessionsStore.getState().currentSessionId === sessionId) {
        sessionStore.getState().setError(friendlyError(e));
      }
      if (strict) throw e;
    } finally {
      // Historical replay can omit its terminal turn update. A replay-created
      // assistant must not keep the Composer in a running state after load.
      // Cached live turns are protected by finalizeIncompleteReplay itself.
      sessionStore.getState().finalizeIncompleteReplay(
        sessionId,
        historyLoaded && findSessionSummary(sessionId)?.status === "completed",
      );
      setLoadingSession((pending) => pending?.generation === generation ? null : pending);
      // Replay window is over: a *new* turn's updates for this session must be
      // ingested again. (No-op when there was no cached transcript to suppress.)
      sessionStore.getState().clearReplaySuppression(sessionId);
    }
  };

  const handleOpenSubagentSession = (sessionId: string, sessionCwd: string | undefined, source: SubagentOpenContext) =>
    handleSelectSession(sessionId, sessionCwd, false, false, { kind: "subagent", source });

  const handleReturnToParentSession = () => {
    const point = activeSubagentReturnPoint(subagentTrail, sessionsStore.getState().currentSessionId);
    if (point) void handleSelectSession(point.parentSessionId, point.parentCwd, false, false, { kind: "return", point });
  };

  notificationNavigatorRef.current = (sessionId) => handleSelectSession(sessionId, undefined, false, true);
  notificationAutomationNavigatorRef.current = async (automationId) => {
    if (placeholderView === "代码开发" && codingLeaveGuardRef.current) {
      if (!(await codingLeaveGuardRef.current())) throw new Error("请先处理代码开发中的未保存内容");
    }
    selectionGenerationRef.current += 1;
    setNotificationAutomationOpen({ id: automationId, sequence: ++notificationAutomationSequenceRef.current });
    setPlaceholderView("自动化");
    sessionsStore.getState().setCurrent(null);
    sessionStore.getState().reset();
  };

  useEffect(() => {
    if (!init) return;
    let disposed = false;
    let running = false;
    let dirty = false;
    let unlisten: (() => void) | undefined;
    const drain = async () => {
      if (running) return;
      running = true;
      try {
        while (dirty && !disposed) {
          dirty = false;
          const opens = await invoke<Array<{ id: string; sessionId?: string; requestId?: string; automationId?: string }>>(
            "notification_take_pending_opens",
          );
          for (const open of opens) {
            if (disposed) return;
            try {
              if (open.sessionId) await notificationNavigatorRef.current(open.sessionId);
              else if (open.automationId) await notificationAutomationNavigatorRef.current(open.automationId);
              else throw new Error("通知缺少关联任务");
              setSettingsOpen(false);
              if (open.requestId && open.sessionId) {
                const permissionState = usePermissionStore.getState();
                const closed = permissionState.closedRequestIds.includes(open.requestId);
                const pending = permissionState.promote(open.requestId, open.sessionId);
                if (closed) showToast("这项授权请求已处理，已打开关联任务", 5000);
                else if (!pending) showToast("已打开关联任务，正在等待授权请求显示", 5000);
              }
              const notificationId = Number(open.id);
              if (/^\d+$/.test(open.id) && Number.isSafeInteger(notificationId)) {
                void notificationMarkRead(notificationId).catch(() => {});
              }
            } catch (error) {
              showToast(`打开通知关联任务失败：${friendlyError(error)}`, 6000);
            }
          }
        }
      } catch (error) {
        showToast(`读取通知跳转失败：${friendlyError(error)}`, 6000);
      } finally {
        running = false;
        if (dirty && !disposed) void drain();
      }
    };
    const requestDrain = () => {
      dirty = true;
      void drain();
    };
    void listen("notification://opened", requestDrain).then((stop) => {
      if (disposed) stop();
      else {
        unlisten = stop;
        requestDrain();
      }
    }).catch((error) => showToast(`监听通知点击失败：${friendlyError(error)}`, 6000));
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [Boolean(init)]);

  // Rewind rewrites the backend history, so our cached transcript is stale —
  // drop it and reload from EchoAgent so the UI matches the rolled-back state.
  const handleRewound = async (rewoundSessionId: string) => {
    const entry = findSessionSummary(rewoundSessionId);
    sessionStore.getState().dropSessionCache(rewoundSessionId);
    const focused = sessionsStore.getState().currentSessionId === rewoundSessionId;
    if (!entry?.cwd) {
      const error = new Error("无法重载会话：缺少工作区信息");
      if (focused) sessionStore.getState().setError(error.message);
      throw error;
    }
    // A replacement can finish after navigation. Reload its transcript in the
    // background without stealing focus so the retry remains complete and the
    // next visit cannot suppress a partial cache as authoritative history.
    if (focused) sessionStore.getState().setSession(rewoundSessionId);
    useSubagentStore.getState().clearSession(rewoundSessionId);
    try {
      await agentLoadSession(rewoundSessionId, entry.cwd);
      if (sessionsStore.getState().currentSessionId === rewoundSessionId) {
        setTaskRefreshSignal((value) => value + 1);
      }
    } catch (error) {
      if (sessionsStore.getState().currentSessionId === rewoundSessionId) {
        sessionStore.getState().setError(friendlyError(error));
      }
      throw error;
    } finally {
      sessionStore.getState().clearReplaySuppression(rewoundSessionId);
    }
  };

  // Fork copies the session to a new id — jump to it so the user sees the
  // branch they just created (and it appears in the sidebar).
  const handleForked = (newId: string, sourceSessionId: string, sourceCwd?: string) => {
    const source = findSessionSummary(sourceSessionId);
    const cwd = source?.cwd ?? sourceCwd;
    if (!cwd) {
      showToast("分叉已创建，但缺少工作区信息，请刷新会话列表");
      void refreshSessionCatalog();
      return;
    }
    const modelId = resolveSessionModelId(models, source?.currentModelId);
    sessionsStore.getState().upsert({
      sessionId: newId,
      title: source?.title ? `${source.title}（分叉）` : "分叉会话",
      cwd,
      currentModelId: modelId,
      permissionMode: source?.permissionMode ?? "ask",
    });
    // A slow fork must not hijack a conversation the user selected meanwhile.
    if (sessionsStore.getState().currentSessionId !== sourceSessionId) return;
    setPlaceholderView(null);
    sessionsStore.getState().setCurrent(newId);
    sessionStore.getState().setSession(newId);
    void agentLoadSession(newId, cwd)
      .then(() => {
        if (sessionsStore.getState().currentSessionId === newId) {
          setTaskRefreshSignal((value) => value + 1);
        }
      })
      .catch((e) => {
        if (sessionsStore.getState().currentSessionId === newId) {
          sessionStore.getState().setError(friendlyError(e));
        }
      });
  };

  /** Execute commands owned by the desktop shell. Runtime commands and Skills
   *  never reach this switch; Composer sends those through ACP unchanged. */
  const handleClientSlashCommand = async ({
    name,
    args,
  }: SlashCommandInvocation): Promise<boolean> => {
    switch (name) {
      case "new":
      case "clear":
        setSettingsOpen(false);
        setSearchOpen(false);
        handleNewSession();
        return true;
      case "search":
      case "history":
        openSearch();
        return true;
      case "help":
        openSettings("help");
        return true;
      case "model":
        openSettings("model");
        return true;
      case "settings": {
        const aliases: Record<string, SettingsSectionId> = {
          "": "model",
          model: "model",
          agent: "agent-settings",
          memory: "memory",
          security: "security",
          help: "help",
          shortcuts: "shortcuts",
          data: "data",
          archived: "archived",
          archive: "archived",
          usage: "usage",
          general: "general",
          notifications: "agent-mail",
        };
        const section = aliases[args.toLowerCase()];
        if (!section) {
          showToast("用法：/settings model|agent|memory|archived|usage|security|help");
          return false;
        }
        openSettings(section);
        return true;
      }
      case "projects":
        handleNavigate("项目");
        return true;
      case "agents":
        handleNavigate("专家·技能·连接器");
        return true;
      case "skills":
        handleNavigate("技能");
        return true;
      case "connectors":
        handleNavigate("连接器");
        return true;
      case "automation":
        handleNavigate("自动化");
        return true;
      case "marketplace":
        handleNavigate("插件市场");
        return true;
      case "usage":
        handleNavigate("用量统计");
        return true;
      case "remember": {
        const remember = parseRememberArguments(args);
        if (!remember) {
          showToast("用法：/remember [global|workspace] <内容>");
          return false;
        }
        const activeId = sessionsStore.getState().currentSessionId;
        const memoryCwd = activeId
          ? findSessionSummary(activeId)?.cwd
          : newSessionTargetCwd;
        if (remember.scope === "workspace" && !memoryCwd) {
          showToast("无法确定当前工作区，记忆未保存");
          return false;
        }
        await memoryAppend(remember.scope, remember.content, memoryCwd || undefined);
        showToast(remember.scope === "global" ? "已保存到全局记忆" : "已保存到当前工作区记忆");
        return true;
      }
      case "plan": {
        const sessionId = sessionsStore.getState().currentSessionId;
        if (!sessionId) {
          showToast("/plan 需要先创建会话");
          return false;
        }
        const normalized = args.toLowerCase();
        if (normalized && !["on", "off", "toggle"].includes(normalized)) {
          showToast("用法：/plan [on|off]");
          return false;
        }
        const current = sessionStore.getState().planMode;
        const enabled = normalized === "on"
          ? true
          : normalized === "off"
            ? false
            : !current;
        await togglePlanMode(sessionId, enabled);
        showToast(enabled ? "已请求开启计划模式" : "已请求关闭计划模式");
        return true;
      }
      case "fork": {
        const sessionId = sessionsStore.getState().currentSessionId;
        if (!sessionId) {
          showToast("/fork 需要先创建会话");
          return false;
        }
        const source = findSessionSummary(sessionId);
        const cwd = source?.cwd;
        if (!cwd) {
          showToast("无法分叉：当前会话缺少工作区信息");
          return false;
        }
        const newId = await sessionFork(sessionId, cwd);
        handleForked(newId, sessionId, cwd);
        showToast(`已分叉到新会话 ${newId.slice(0, 8)}`);
        return true;
      }
      case "rename": {
        const sessionId = sessionsStore.getState().currentSessionId;
        const entry = sessionId ? findSessionSummary(sessionId) : undefined;
        if (!sessionId || !entry) {
          showToast("/rename 需要当前会话");
          return false;
        }
        if (!args.trim()) {
          showToast("用法：/rename <新标题>");
          return false;
        }
        await agentRenameSession(sessionId, args.trim(), entry.cwd);
        sessionsStore.getState().upsert({ sessionId, title: args.trim() });
        showToast("会话已重命名");
        return true;
      }
      default:
        // The command catalog and this executor are deliberately kept in one
        // typed contract; returning false preserves the user's input if a new
        // command is added without its action being wired.
        showToast(`未实现的桌面命令：/${name}`);
        return false;
    }
  };

  // Select an expert from the + menu (chat or home composer). Instead of
  // immediately creating a session, set the pending expert and go home so the
  // user can type their message with the expert badge visible.
  const handleStartWithExpert = (
    agent: AgentEntry,
    _meta?: { expertId?: string; source?: string },
  ) => {
    const promptBody = agent.raw
      ? extractMarkdownBody(agent.raw)
      : agent.description ?? "";
    usePendingExpertStore.getState().set({
      name: agent.name,
      prompt: promptBody,
      description: agent.description ?? agent.name,
      expertId: _meta?.expertId ?? agent.name,
      source: _meta?.source ?? agent.scope ?? "local",
    });
    handleGoHome();
  };

  // 进入本地项目：在项目关联目录中新建会话，并注入项目说明作为种子消息。
  const handleStartProject = async (project: ProjectMeta) => {
    const modelId = project.defaultModelId
      ? requireExplicitProjectModel(project.defaultModelId)
      : requireConfiguredModel();
    if (!modelId) return;
    if (!ensureQuotaAllowsSend()) return;
    let startedSessionId: string | undefined;
    try {
      setPlaceholderView(null);
      const cwd = project.cwd || newSessionTargetCwd;
      const sessionId = await agentNewSession(cwd, modelId);
      startedSessionId = sessionId;
      useKnowledgeStore.getState().bindSessionSources(sessionId, true);
      if (!project.defaultModelId) {
        useProjectsStore.getState().updateConfig(project.id, { defaultModelId: modelId });
      }
      setCurrentModelId(modelId);
      sessionsStore.getState().setCurrent(sessionId);
      sessionsStore.getState().upsert({
        sessionId,
        title: project.name,
        cwd,
        status: "working",
        currentModelId: modelId,
        permissionMode: "ask",
      });
      sessionStore.getState().setSession(sessionId);
      // Register the session as a project conversation.
      useProjectsStore.getState().addConversation(project.id, {
        sessionId,
        title: project.name,
        createdAt: new Date().toISOString(),
        modelId,
      });
      const seed = `开始「${project.name}」项目，请先根据项目配置确认目标、约束和下一步。`;
      const promptId = createAgentPromptId();
      sessionStore.getState().pushUser(seed);
      sessionStore.getState().startStreaming(undefined, promptId);
      await agentSend(sessionId, buildProjectPrompt(project, seed), [], seed, promptId);
    } catch (e) {
      if (startedSessionId) {
        applySessionScopedFailure({
          failedSessionId: startedSessionId,
          currentSessionId: sessionStore.getState().sessionId,
          message: friendlyError(e),
          setStatus: (sessionId, status) => sessionsStore.getState().upsert({ sessionId, status }),
          setCurrentError: (message) => sessionStore.getState().setError(message),
        });
      }
      showToast(`启动项目失败：${friendlyError(e)}`);
    }
  };

  // 在项目中新建对话（从侧栏 + 按钮或项目详情页 Composer 触发）。
  // 创建 EchoAgent 会话 → 注册到项目 conversations → 打开 ChatView → 可选发送首条消息。
  const handleStartProjectConversation = async (
    projectId: string,
    message?: string,
    requestedModelId?: string,
    attachments: string[] = [],
  ) => {
    const project = useProjectsStore.getState().projects.find((p) => p.id === projectId);
    if (!project) return;
    const persistedModelId = requestedModelId || project.defaultModelId;
    let modelId: string | undefined;
    if (persistedModelId) {
      modelId = requireExplicitProjectModel(persistedModelId);
    } else if (message) {
      showToast("请先在项目中选择本次任务使用的模型");
    } else {
      modelId = requireConfiguredModel();
    }
    if (!modelId) return;
    if (message && !ensureQuotaAllowsSend()) return;
    let startedSessionId: string | undefined;
    try {
      const cwd = project.cwd || newSessionTargetCwd;
      const sessionId = await agentNewSession(cwd, modelId);
      startedSessionId = sessionId;
      useKnowledgeStore.getState().bindSessionSources(sessionId, true);
      if (!project.defaultModelId && !requestedModelId) {
        useProjectsStore.getState().updateConfig(projectId, { defaultModelId: modelId });
      }

      const title = message ? deriveTitle(message) : `${project.name} 对话`;

      // Register conversation in the project.
      useProjectsStore.getState().addConversation(projectId, {
        sessionId,
        title,
        createdAt: new Date().toISOString(),
        modelId,
      });

      // Navigate to chat view.
      setPlaceholderView(null);
      setCurrentModelId(modelId);
      sessionsStore.getState().setCurrent(sessionId);
      sessionsStore.getState().upsert({
        sessionId,
        title,
        cwd,
        status: message ? "working" : "pending",
        currentModelId: modelId,
        permissionMode: "ask",
      });
      sessionStore.getState().setSession(sessionId);

      if (message) {
        // Every new runtime session needs the project contract; previous
        // project conversations do not share an ACP context window.
        const prompt = buildProjectPrompt(project, message);
        const promptId = createAgentPromptId();
        sessionStore.getState().pushUser(message);
        sessionStore.getState().startStreaming(undefined, promptId);
        await agentSend(sessionId, prompt, attachments, message, promptId);
      }
      return sessionId;
    } catch (e) {
      if (startedSessionId) {
        applySessionScopedFailure({
          failedSessionId: startedSessionId,
          currentSessionId: sessionStore.getState().sessionId,
          message: friendlyError(e),
          setStatus: (sessionId, status) => sessionsStore.getState().upsert({ sessionId, status }),
          setCurrentError: (message) => sessionStore.getState().setError(message),
        });
      }
      showToast(`创建项目对话失败：${friendlyError(e)}`);
      return undefined;
    }
  };

  const activeNav = placeholderView ?? (currentSessionId ? "" : "新建任务");
  const activeSubagentParent = activeSubagentReturnPoint(subagentTrail, currentSessionId);
  const parentSessionTitle = activeSubagentParent
    ? findSessionSummary(activeSubagentParent.parentSessionId)?.title ?? "上级任务"
    : null;

  const codingWorkspaceActive = placeholderView === "代码开发";
  useEffect(() => {
    if (codingWorkspaceActive) setHasOpenedCodingWorkspace(true);
  }, [codingWorkspaceActive]);
  const codingCwd = activeCodingWorkspaceCwd || codingWorkspaceCwd
    || (codingWorkspaceActive ? activeSessionCwd || newSessionTargetCwd : "");
  const placeholderProps = {
    onNavigate: handleNavigate,
    onOpenSession: handleSelectSession,
    onGoHome: handleGoHome,
    createExpertRequested,
    onCreateExpertRequestHandled: () => setCreateExpertRequested(false),
    onStartOrganizationConversation: handleStartOrganizationConversation,
    onToast: showToast,
    codingWorkspaces,
    activeCodingWorkspaceCwd,
    onCloseCodingWorkspace: handleCloseCodingWorkspace,
    workspaces,
    sessionId: currentSessionId ?? undefined,
    notificationAutomationId: notificationAutomationOpen?.id,
    notificationAutomationSequence: notificationAutomationOpen?.sequence,
    codingApiReady: !!init?.auth.ready && !!newSessionModelId,
    codingModels: models,
    codingModelId: activeSessionModelId ?? newSessionModelId,
    projectModels: models,
    projectDefaultModelId: newSessionModelId,
    meetingModelId: meetingLaunchModelId ?? activeSessionModelId ?? newSessionModelId,
    meetingModels: models,
    onOpenMeetingMinutes: (modelId?: string) => {
      setMeetingLaunchModelId(modelId);
      handleNavigate("录音转写");
    },
    onOpenModelSettings: () => openSettings("model"),
    onClientSlashCommand: handleClientSlashCommand,
    onExitCodingWorkspace: () => {
      setPlaceholderView(null);
      setSidebarCollapsed(false);
    },
    onRegisterCodingLeaveGuard: registerCodingLeaveGuard,
    onStartCodingRun: handleStartCodingRun,
    onActivateCodingSession: (targetSessionId: string, targetCwd: string) =>
      handleSelectSession(targetSessionId, targetCwd, true),
    onChangeCodingModel: handleModelChange,
    onSendCodingMessage: (value: string, promptTextOverride?: string) =>
      handleSendCurrent(value, [], undefined, promptTextOverride),
    onCancelCodingRun: () => handleCancel("stop"),
    onStartProject: handleStartProject,
    onStartProjectConversation: handleStartProjectConversation,
    onRenameSession: handleRenameSession,
    onArchiveSession: handleArchiveSession,
    onDeleteSession: handleDeleteSession,
    automationRefreshSignal,
  };

  return (
    <div className={`app${IS_MACOS ? " app--macos" : ""}${codingWorkspaceActive ? " app--coding" : ""}`}>
      {init?.ok && <CodingTaskRuntimeManager roots={[...new Set([
        ...codingWorkspaces.map((workspace) => workspace.cwd),
        codingWorkspaceCwd,
        activeCodingWorkspaceCwd,
      ].filter(Boolean))]} />}
      {/* macOS 使用系统原生 Overlay 标题栏(红绿灯 + 原生菜单栏),
          不再渲染自绘 TitleBar;Windows 保留窗口控制但隐藏左侧菜单区。 */}
      {!IS_MACOS && (
        <TitleBar
          onPlaceholder={handlePlaceholder}
          onShowAbout={openAbout}
          onCheckForUpdates={handleCheckForUpdates}
          hideMenus={IS_WINDOWS}
        />
      )}
      <SidebarLayoutContext.Provider value={sidebarLayout.context}>
      <div ref={sidebarLayout.bodyRef} style={sidebarLayout.style} className={"app__body" + (sidebarCollapsed ? " app__body--collapsed" : "") + (sidebarLayout.resizing ? " app__body--sidebar-resizing" : "")}>
        <Sidebar
          onNewSession={handleNewSession}
          onSelect={handleSelectSession}
          onNavigate={handleNavigate}
          onOpenSettings={() => openSettings("model")}
          onToggleCollapse={() => setSidebarCollapsed(true)}
          onOpenSearch={openSearch}
          onPlaceholder={handlePlaceholder}
          onToast={showToast}
          onOpenProject={handleOpenProjectFromSidebar}
          onStartProjectConversation={handleStartProjectConversation}
          onRenameSession={handleRenameSession}
          onArchiveSession={handleArchiveSession}
          onDeleteSession={handleDeleteSession}
          onRetrySessions={() => void refreshSessionCatalog()}
          activeNav={activeNav}
        />
        <SidebarResizeHandle layout={sidebarLayout} />
        <main className="app__main">
          {/* 对话页使用会话顶栏。侧栏折叠后的其它页面使用占位的紧凑工具栏，
              避免展开/新建按钮覆盖各页面自己的标题和导航。
              注:Tauri 2 只认 data-tauri-drag-region(CSS 的 -webkit-app-region
              不生效);按钮等子元素不是拖拽目标,不影响点击。 */}
          {!placeholderView && currentSessionId ? (
            <header className="main-topbar" data-tauri-drag-region>
              <div className="main-topbar__left">
                {sidebarCollapsed && (
                  <>
                    <button
                      className="main-topbar__btn"
                      aria-label="展开侧边栏"
                      data-tip="展开侧边栏"
                      onClick={() => setSidebarCollapsed(false)}
                    >
                      <SidebarToggleIcon size="md" />
                    </button>
                    <button
                      className="main-topbar__btn"
                      aria-label="新建任务"
                      data-tip="新建任务"
                      onClick={handleNewSession}
                    >
                      <EchoNewTaskIcon size="md" />
                    </button>
                  </>
                )}
                {activeSubagentParent && (
                  <button type="button" className="main-topbar__return"
                    onClick={handleReturnToParentSession}
                    aria-label={`返回上级任务：${parentSessionTitle}`}
                    title={`返回上级任务：${parentSessionTitle}`}>
                    <ChevronLeftIcon size="sm" />
                    <span>返回上级任务</span>
                  </button>
                )}
                <TopbarTitle key={currentSessionId ?? "new"} title={currentTitle} onRename={handleRenameTitle} />
                {currentEntry?.expertName && (
                  <span className="expert-badge" data-tip={`专家：${currentEntry.expertName}`}>
                    <ThumbImg name={currentEntry.expertName} local={currentEntry.expertAvatar} size={18} shape="circle" />
                    {currentEntry.expertName}
                  </span>
                )}
                {currentSessionId && (
                  <TopbarActions
                    sessionId={currentSessionId}
                    title={currentTitle}
                    pinned={currentEntry?.pinned}
                    onToast={showToast}
                    onSessionsChanged={(patch) => {
                      if (patch) sessionsStore.getState().upsert({ sessionId: currentSessionId, ...patch });
                    }}
                    onArchived={(archived) => {
                      syncArchivedSessionState(currentSessionId, archived);
                      if (archived) showArchivedSessionActions(currentSessionId);
                    }}
                  />
                )}
              </div>
            </header>
          ) : (
            sidebarCollapsed && !codingWorkspaceActive && (
              <header className="main-page-controls" data-tauri-drag-region>
                <button
                  className="main-topbar__btn"
                  aria-label="展开侧边栏"
                  data-tip="展开侧边栏"
                  onClick={() => setSidebarCollapsed(false)}
                >
                  <SidebarToggleIcon size="md" />
                </button>
                <button
                  className="main-topbar__btn"
                  aria-label="新建任务"
                  data-tip="新建任务"
                  onClick={handleNewSession}
                >
                  <EchoNewTaskIcon size="md" />
                </button>
              </header>
            )
          )}
          {init?.ok && modelCatalogError && (
            <div className="app__notice app__notice--err" role="alert">
              模型列表加载失败：{modelCatalogError}
              <button type="button" className="btn btn--ghost" onClick={() => void refreshModels()}>
                重试
              </button>
            </div>
          )}
          {init?.ok && (codingWorkspaceActive || hasOpenedCodingWorkspace) && (
            <div className="app__coding-surface" hidden={!codingWorkspaceActive}>
              <Suspense fallback={<div className="app__notice" role="status">正在加载界面…</div>}>
                <PlaceholderPage
                  {...placeholderProps}
                  label="代码开发"
                  cwd={codingCwd}
                  onSelectWorkspace={handleSelectCodingWorkspace}
                />
              </Suspense>
            </div>
          )}
          {initError ? (
            <div className="app__notice app__notice--err">
              初始化失败:{initError}
              <br />
              <button
                type="button"
                className="btn btn--primary"
                onClick={() => {
                  setInit(null);
                  setInitError(null);
                  setInitAttempt((attempt) => attempt + 1);
                }}
              >
                重试初始化
              </button>
            </div>
          ) : !init ? (
            <div className="app__notice">正在本地初始化 agent…</div>
          ) : !init.ok ? (
            <div className="app__notice app__notice--err">
              EchoAgent 未就绪:{init.auth.reason ?? "未知原因"}
              <br />
              请在「设置 → 模型」配置模型厂商和 API Key。
            </div>
          ) : codingWorkspaceActive ? null : (
            <Suspense fallback={<div className="app__notice" role="status">正在加载界面…</div>}>
              {placeholderView ? (
                <PlaceholderPage
                  {...placeholderProps}
                  label={placeholderView}
                  cwd={newSessionTargetCwd}
                  onSelectWorkspace={handleSelectWorkspace}
                />
              ) : currentSessionId ? (
                <ChatView
                  title={currentTitle}
                  onSend={handleSendCurrent}
                  onSendNow={handleSendNowCurrent}
                  onCancel={handleCancel}
                  cancelling={cancellingSessionId === currentSessionId}
                  apiReady={chatReady}
                  setupHint={chatSetupHint}
                  onOpenSettings={!sessionLoading && !modelSwitching && (!init.auth.ready || models.length === 0)
                    ? () => openSettings("model")
                    : undefined}
                  modelId={activeSessionModelId}
                  modelLoading={sessionLoading || modelSwitching}
                  models={models}
                  onModelChange={handleModelChange}
                  cwd={activeSessionCwd}
                  newSessionTargetCwd={newSessionTargetCwd}
                  workspaces={workspaces}
                  onSelectWorkspace={handleSelectWorkspace}
                  onPrepareRetry={handlePrepareRetry}
                  onRetrySend={handleRetrySend}
                  onRewound={handleRewound}
                  onForked={handleForked}
                  onToast={showToast}
                  onSelectExpert={handleStartWithExpert}
                  onOpenSubagentSession={handleOpenSubagentSession}
                  subagentScrollRestore={subagentScrollRestore}
                  onNavigateConnectors={() => setPlaceholderView("专家·技能·连接器")}
                  onOpenKnowledgeBase={() => handleNavigate("知识库")}
                  onOpenMeetingMinutes={() => {
                    setMeetingLaunchModelId(activeSessionModelId);
                    handleNavigate("录音转写");
                  }}
                  onOpenOrganization={() => handleNavigate("组织")}
                  commandRefreshKey={commandRefreshKey}
                  onClientSlashCommand={handleClientSlashCommand}
                />
              ) : (
                <HomePage
                  onSend={handleSendNew}
                  streaming={streaming}
                  apiReady={init.auth.ready && modelConfigured}
                  modelLoading={modelCatalogLoading && models.length === 0}
                  setupHint={init.auth.reason}
                  creatingSession={creatingSession}
                  sendError={homeSendError}
                  onOpenSettings={() => openSettings("model")}
                  onPlaceholder={handlePlaceholder}
                  modelId={currentModelId}
                  models={models}
                  onModelChange={handleModelChange}
                  cwd={newSessionTargetCwd}
                  workspaces={workspaces}
                  onSelectWorkspace={handleSelectWorkspace}
                  onSelectExpert={handleStartWithExpert}
                  onNavigateConnectors={() => setPlaceholderView("专家·技能·连接器")}
                  onOpenKnowledgeBase={() => handleNavigate("知识库")}
                  onOpenMeetingMinutes={() => {
                    setMeetingLaunchModelId(currentModelId);
                    handleNavigate("录音转写");
                  }}
                  onOpenOrganization={() => handleNavigate("组织")}
                  commandRefreshKey={commandRefreshKey}
                  onClientSlashCommand={handleClientSlashCommand}
                  taskMode={newTaskMode}
                  onTaskModeChange={setNewTaskMode}
                />
              )}
            </Suspense>
          )}
        </main>
      </div>
      </SidebarLayoutContext.Provider>
      {toasts.map((toast, index) => (
        <Toast
          key={toast.id}
          message={toast.message}
          actions={toast.actions}
          offset={toasts.length - 1 - index}
          onDismiss={() => dismissToast(toast.id)}
        />
      ))}
      <Suspense fallback={null}>
        <MeetingRecordingIndicator
          onOpen={(modelId) => {
            setMeetingLaunchModelId(modelId);
            handleNavigate("录音转写");
          }}
          onToast={showToast}
        />
      </Suspense>
      {searchOpen && (
        <Suspense fallback={null}>
          <SearchOverlay
            open
            onClose={() => setSearchOpen(false)}
            onSelect={handleSelectSession}
          />
        </Suspense>
      )}
      {settingsOpen && (
        <Suspense fallback={null}>
          <SettingsPanel
            open
            initialSection={settingsSection}
            sessionId={currentSessionId ?? undefined}
            cwd={activeSessionCwd || newSessionTargetCwd}
            onClose={() => setSettingsOpen(false)}
            onModelsChanged={refreshModels}
            onRestoreSession={handleArchiveSession}
            onDeleteSession={handleDeleteSession}
            onOpenSession={(sessionId) => handleSelectSession(sessionId, undefined, false, true)}
            onOpenAutomation={(automationId) => notificationAutomationNavigatorRef.current(automationId)}
            onToast={showToast}
          />
        </Suspense>
      )}
      {aboutOpen && (
        <Suspense fallback={null}>
          <AboutDialog
            open
            onClose={() => setAboutOpen(false)}
            init={init}
            onCheckForUpdates={handleCheckForUpdates}
          />
        </Suspense>
      )}
      {updateDialogOpen && (
        <Suspense fallback={null}>
          <UpdateDialog open onClose={() => setUpdateDialogOpen(false)} />
        </Suspense>
      )}
      <Suspense fallback={null}>
        <FolderTrustDialog
          request={trustRequest}
          onResolve={resolveTrustRequest}
          onToast={showToast}
        />
      </Suspense>
      <TasksPanel
        sessionId={currentSessionId ?? undefined}
        refreshSignal={taskRefreshSignal}
        onToast={showToast}
      />
      <SecondarySidebar onSelectExpert={handleStartWithExpert} onCreateExpert={handleCreateExpert} onToast={showToast} />
    </div>
  );
}
