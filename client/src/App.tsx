import type { CodingTurnOptions } from "./lib/agent-client";
import { codingApi } from "./features/coding/lib/tauri-api";
import { useTaskStore } from "./features/coding/store/task-store";
import { findModelDisplayName } from "@/lib/model-presentation";
import { SessionNavigation } from "./components/SessionHeader";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { lazy, Suspense, useCallback, useEffect, useRef, useState } from "react";
import { TitleBar } from "./components/TitleBar";
import { Sidebar } from "./components/Sidebar";
import { SidebarResizeHandle } from "./components/SidebarResizeHandle";
import { SidebarLayoutContext, useAppSidebarLayout } from "./lib/sidebar-layout";
import { HomePage } from "./components/HomePage";
import { PlaceholderPage } from "./components/PlaceholderPage";
import { CapabilitySection } from "./components/CapabilitySection";
import { isCapabilityView } from "./lib/capability-navigation";
import { Toast, type ToastAction } from "./components/Toast";
import { useToastQueue } from "./lib/use-toast-queue";
import { useRendererRecoveryStatus, type RendererRecoveryStatus } from "./lib/renderer-recovery";
import { readRecoverySessionId, saveRecoverySessionId } from "./lib/recovery-context";
import { planRecoveryAttempt, MAX_RECOVERY_ATTEMPTS } from "./lib/recovery-policy";
import { toastMessage, type ToastMessageKind } from "./lib/toast-message";
import { memoryDreamMessage, memoryFlushMessage } from "./lib/memory-feedback";
// PermissionDialog is now inline in ChatView (PermissionInlineCard), not a global modal.
import { ThemeProvider } from "./components/ThemeProvider";
import type { SettingsSectionId } from "./components/SettingsPanel";

import { ChevronLeftIcon } from "./foundation/components/Icon/icons";
import type { ModelOption } from "./components/ModelSelector";
import {
  isConfiguredModelId,
  resolveConfiguredModelId,
  resolveSessionModelId,
} from "./lib/model-selection";
import { useSessionStore } from "./stores/session-store";
import { useSessionsStore } from "./stores/sessions-store";
import { reconcileSessionCatalog } from "./lib/session-catalog";
import { meetingDrafts } from "./lib/meeting-drafts";
import { createExitPreparationController, type DesktopExitNotice, type DesktopExitFailure } from "./lib/recording-exit";
import { usePermissionStore } from "./stores/permission-store";
import {
  hasPendingQuestionForSession,
  useQuestionStore,
} from "./stores/question-store";
import { usePendingExpertStore } from "./stores/pending-expert-store";

import {
  agentInit,
  agentNewSession,
  agentPrepareTaskWorkspace,
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
  memoryRecordSave,
  memoryFlush,
  memoryDream,
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
import { codingQueueOwner, settleCodingQueueTurn } from "./features/coding/lib/coding-followup";
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
import { resolveGroupForSession } from "./features/groups/group-client";
import { useGroupEvents, useGroupStore } from "./features/groups/group-store";
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
import { useAppDialog } from "./components/AppDialog";
import { confirmAutomationEditorExit } from "./stores/automation-editor-store";
import { hydrateKnowledgeSources } from "./lib/kb-source-storage";
import {
  permissionModeStatusFromEvent,
  usePermissionModeStore,
} from "./stores/permission-mode-store";
import { agentsValidateReferences } from "./lib/agent-client";
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
import { GroupActionsHost } from "./features/groups/GroupActions";
const GroupWorkspace = lazy(() => import("./features/groups/GroupWorkspace").then(module => ({ default: module.GroupWorkspace })));
const SettingsPanel = lazy(() => import("./components/SettingsPanel").then((module) => ({ default: module.SettingsPanel })));
const SearchOverlay = lazy(() => import("./components/SearchOverlay").then((module) => ({ default: module.SearchOverlay })));
const AboutDialog = lazy(() => import("./components/AboutDialog").then((module) => ({ default: module.AboutDialog })));
const UpdateDialog = lazy(() => import("./components/UpdateDialog").then((module) => ({ default: module.UpdateDialog })));
const FolderTrustDialog = lazy(() => import("./components/FolderTrustDialog").then((module) => ({ default: module.FolderTrustDialog })));
const MeetingRecordingIndicator = lazy(() => import("./components/MeetingRecordingIndicator").then((module) => ({ default: module.MeetingRecordingIndicator })));

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
      // The shell has committed. Its native readiness handshake must also run
      // when WebKit suspends animation frames for an occluded test window.
      const timer = setTimeout(() => { void invoke("desktop_validation_ready").catch(console.error); }, 0);
      return () => clearTimeout(timer);
    }
  }, [init?.ok]);

  const [initError, setInitError] = useState<string | null>(null);
  useGroupEvents(!!init?.ok);
  const [initAttempt, setInitAttempt] = useState(0);
  const [exitPreparing, setExitPreparing] = useState(false);
  const runtimeRecoveryRequested = useRef(false);
  const [recoverySessionId] = useState(readRecoverySessionId);
  const rendererRestoreChecked = useRef(false);
  const rendererHistoryPending = useRef(false);
  const runtimeRecoveryAttempts = useRef<number[]>([]);
  const runtimeRecoverySuppressed = useRef(false);
  const [runtimeAutoRetry, setRuntimeAutoRetry] = useState<number | null>(null);
  const rendererRecovery = useRendererRecoveryStatus();
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
  const { toasts, enqueueToast, dismissToast, pauseToast } = useToastQueue();
  useEffect(() => {
    if (rendererRecovery?.phase === "restored") {
      enqueueToast(rendererRecovery.message, [], 8000);
    }
  }, [rendererRecovery?.phase, rendererRecovery?.windowGeneration, rendererRecovery?.attempt, enqueueToast]);

  useEffect(() => {
    if (!initError || !runtimeRecoveryRequested.current || exitPreparing || runtimeRecoverySuppressed.current) {
      setRuntimeAutoRetry(null);
      return;
    }
    const plan = planRecoveryAttempt(runtimeRecoveryAttempts.current, Date.now());
    if (!plan) { setRuntimeAutoRetry(null); return; }
    setRuntimeAutoRetry(plan.attempt);
    const timer = setTimeout(() => {
      runtimeRecoveryAttempts.current = [...plan.attempts, Date.now()];
      setInit(null);
      setInitError(null);
      setInitAttempt((value) => value + 1);
    }, plan.delay);
    return () => clearTimeout(timer);
  }, [initError, exitPreparing]);

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
  const [automaticWorkspace, setAutomaticWorkspace] = useState(true);
  const taskWorkspaceRequestRef = useRef(crypto.randomUUID());
  const projectSessionRequestsRef = useRef(new Map<string, { context: string; requestId: string; pending: boolean }>());
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
  const modelsRef = useRef<ModelOption[]>([]);
  const authReadyRef = useRef(false);
  const promptedUpdateVersionRef = useRef<string | null>(null);
  const newSessionPendingRef = useRef(false);
  const selectionGenerationRef = useRef(0);
  const sessionCatalogGenerationRef = useRef(0);
  const modelCatalogGenerationRef = useRef(0);
  const memoryCommandsInFlightRef = useRef(new Set<string>());

  const showToast = useCallback((message: string, durationMs?: number, kind: ToastMessageKind = "auto") => {
    enqueueToast(message, [], durationMs, kind);
  }, [enqueueToast]);

  const sessionStore = useSessionStore;
  const sessionsStore = useSessionsStore;
  const permissionStore = usePermissionStore;
  const questionStore = useQuestionStore;
  const updateStatus = useUpdateStore((state) => state.status);
  const availableUpdateVersion = useUpdateStore((state) => state.update?.version);
  const modalOpen = useModalPresence();
  const { confirm: confirmAutomationExit, dismiss: dismissAutomationExit, dialog: automationExitDialog } = useAppDialog();

  const openSettings = useCallback((section: SettingsSectionId = "general") => {
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
      enqueueToast("暂时无法加载知识库，请稍后重试");
    });
    void hydrateProjectsFromBackend()
      .then(() => {
        useProjectsStore.getState().reconcileSessionArchiveStates(
          useSessionsStore.getState().independent,
        );
      })
      .catch((error) => {
        console.error("[EchoAgent] Failed to hydrate projects:", error);
        enqueueToast("项目暂时无法同步，当前显示上次保存的内容");
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
    const catalogAtStart = store.independent;
    store.setLoading(true);
    store.setError(null);
    const [sessionResult, workspaceResult] = await Promise.allSettled([
      agentListAllSessions(true),
      agentListWorkspaces(),
    ]);
    if (sessionCatalogGenerationRef.current !== generation) return;
    const failures: string[] = [];
    if (sessionResult.status === "fulfilled") {
      const reconciled = reconcileSessionCatalog(sessionResult.value, catalogAtStart, sessionsStore.getState().independent);
      sessionsStore.getState().setIndependent(reconciled);
      useProjectsStore.getState().reconcileSessionArchiveStates(reconciled);
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
      const selectionAtStart = selectionGenerationRef.current;
      try {
        const result = await agentInit();
        if (disposed) return;
        // A new document has no React/Zustand selection. Restore identity from
        // its checkpoint only after native recovery, never on an ordinary boot
        // and never over a newer user navigation. Do not replay the prompt.
        if (recoverySessionId && !rendererRestoreChecked.current && !runtimeRecoveryRequested.current) {
          const recovery = await invoke<RendererRecoveryStatus | null>("desktop_recovery_status").catch(() => null);
          if (disposed) return;
          if ((recovery?.phase === "restored" || recovery?.phase === "recovering")
              && selectionGenerationRef.current === selectionAtStart
              && !sessionsStore.getState().currentSessionId) {
            const catalog = await agentListAllSessions(true);
            if (disposed) return;
            if (selectionGenerationRef.current === selectionAtStart && !sessionsStore.getState().currentSessionId) {
              const entry = catalog.find((session) => session.sessionId === recoverySessionId && !session.archived
                && !sessionsStore.getState().deletedSessionIds.includes(session.sessionId));
              if (entry) {
                sessionsStore.getState().mergeSessions(catalog);
                sessionsStore.getState().setCurrent(entry.sessionId);
                rendererHistoryPending.current = true;
                runtimeRecoveryRequested.current = true;
              } else {
                saveRecoverySessionId(null);
                enqueueToast("界面已恢复，原任务已删除、归档或无法访问，请从任务列表选择其他任务。", [], 8000);
              }
            }
          }
          rendererRestoreChecked.current = true;
        }
        // EchoAgent rejects an empty cwd, so every session needs an absolute
        // execution directory. It does not determine sidebar ownership: all
        // sessions enter the catalog and explicit project references determine
        // whether the sidebar presents one under 任务 or 项目.
        if (disposed) return;
        setNewSessionTargetCwd(result.cwd);
        sessionsStore.getState().setHomeCwd(result.cwd);
        if (!runtimeRecoveryRequested.current) {
          setInit(result);
          authReadyRef.current = result.auth.ready;
        }

        const pendingInteractionStatus = (sessionId: string) => {
          const pending = [
            ["awaiting_permission", !!permissionStore.getState().queues[sessionId]?.length],
            ["awaiting_answer", hasPendingQuestionForSession(sessionId)],
            ["awaiting_approval", !!sessionStore.getState().transcripts[sessionId]?.planApprovals.length],
          ] as const;
          const current = findSessionSummary(sessionId)?.status;
          // Keep the currently presented interaction until it closes, then
          // surface another required action before reporting running again.
          return pending.find(([status, waiting]) => waiting && status === current)?.[0]
            ?? pending.find(([, waiting]) => waiting)?.[0];
        };
        const stopListening = await subscribeAgentEvents({
          onUpdate: (u) => {
            const updateType = (u as { sessionUpdate?: string; type?: string }).sessionUpdate
              ?? (u as { type?: string }).type;
            const updateSessionId = (u as { __sessionId?: string }).__sessionId;
            if (updateSessionId && sessionsStore.getState().deletedSessionIds.includes(updateSessionId)) return;
            const applied = sessionStore.getState().applyUpdate(u);
            const updatedTranscript = updateSessionId ? sessionStore.getState().transcripts[updateSessionId] : undefined;
            const activePromptId = updatedTranscript?.pendingSendNowPromptId
              ?? updatedTranscript?.messages.find(message => message.id === updatedTranscript.streamingMessageId)?.promptId;
            const updatePromptId = typeof u._meta?.promptId === "string" ? u._meta.promptId : undefined;
            // Historical tool results may still enrich their own cards. That
            // does not give them authority to resume the current task's UI.
            const currentPromptUpdate = !updatePromptId || (
              !updatedTranscript?.completedPromptIds?.includes(updatePromptId)
              && (!activePromptId || updatePromptId === activePromptId)
            );
            const pendingStatus = updateSessionId ? pendingInteractionStatus(updateSessionId) : undefined;
            const currentStatus = updateSessionId
              ? findSessionSummary(updateSessionId)?.status
              : undefined;
            if (
              updateSessionId
              && applied && currentPromptUpdate && !updatedTranscript?.control && !pendingStatus
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
              && sessionStore.getState().transcripts[updateSessionId]?.planApprovals.some(
                request => request.requestId === (u as { requestId?: string }).requestId,
              )
            ) {
              sessionsStore.getState().upsert({
                sessionId: updateSessionId,
                status: "awaiting_approval",
                updatedAt: new Date().toISOString(),
              });
            } else if (updateSessionId && updateType === "plan_approval_resolved"
              && currentStatus === "awaiting_approval" && !updatedTranscript?.control) {
              sessionsStore.getState().upsert({
                sessionId: updateSessionId,
                status: pendingStatus ?? "working",
                updatedAt: new Date().toISOString(),
              });
            }
            if (updateType === "available_commands_update") {
              setCommandRefreshKey((value) => value + 1);
            }
            // Background memory processing is visible in Memory's health view.
            // Buttons and desktop /flush /dream commands await their own result.
          },
          onPermission: (p) => {
            if (sessionsStore.getState().deletedSessionIds.includes(p.sessionId)) return;
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
                status: pendingInteractionStatus(sessionId) ?? "working",
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
            if (sessionsStore.getState().deletedSessionIds.includes(p.sessionId)) return;
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
            // Retain the legacy coding identity before completion can discard
            // an empty assistant placeholder.
            const before = sessionStore.getState().transcripts[p.sessionId];
            const codingPromptId = p.promptId || before?.messages.find(
              message => message.id === before.streamingMessageId,
            )?.promptId;
            // Completion is routed by session id in the transcript store. This
            // also finalizes a background conversation after the user switches
            // away; side-channel sessions are not added to the sidebar because
            // they have no SessionSummary entry.
            const completedCurrentTurn = sessionStore.getState().markComplete(p);
            const currentTranscript = sessionStore.getState().transcripts[p.sessionId];
            // A scoped old event may settle its own row. An unscoped event
            // rejected by the transcript must never consume a replacement row.
            if (p.promptId || completedCurrentTurn) {
              settleCodingQueueTurn({ ...p, promptId: codingPromptId ?? "" });
              useMessageQueueStore.getState().settleSending(
                p.sessionId,
                queuePolicy.settlement,
                p.promptId || undefined,
              );
            }
            // An old/duplicate terminal event may enrich its own message, but
            // cannot settle the replacement turn's UI or dispatch more work.
            if (!completedCurrentTurn || currentTranscript?.streamingMessageId
              || currentTranscript?.pendingSendNowPromptId) return;
            if (!queuePolicy.autoAdvance || requestedAction) {
              useMessageQueueStore.getState().pauseQueued(p.sessionId);
            }
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
              || requestedAction
              || !queuePolicy.autoAdvance
              || pendingInteractionStatus(p.sessionId)
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
                  const current = sessionStore.getState().transcripts[p.sessionId];
                  if (!current?.streamingMessageId && !current?.pendingSendNowPromptId) {
                    useMessageQueueStore.getState().pauseQueued(p.sessionId);
                  }
                  if (sessionStore.getState().sessionId === p.sessionId) {
                    sessionStore.getState().setError(detail, { sessionId: p.sessionId, promptId: queuedPromptId });
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
            if (sessionsStore.getState().deletedSessionIds.includes(sessionId)) return;
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
            if (sessionsStore.getState().deletedSessionIds.includes(q.sessionId)) return;
            if (sessionStore.getState().transcripts[q.sessionId]?.control) return;
            if (questionStore.getState().closedRequestIds.includes(q.requestId)) return;
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
                status: pendingInteractionStatus(sessionId) ?? "working",
                updatedAt: new Date().toISOString(),
              });
            }
          },
          onAgentDied: ({ reason }) => {
            console.error('[EchoAgent] Agent thread died:', reason);
            // A replacement Runtime has no loaded sessions, even though the
            // renderer retains their transcripts and catalog entries.
            runtimeRecoveryRequested.current = true;
            const message = `AI 引擎异常退出：${reason}。已有会话会保留；未完成操作不会自动重放。`;
            setInitError(message);
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
            const transcript = sessionStore.getState().transcripts[e.sessionId];
            const active = transcript?.messages.find(message => message.id === transcript.streamingMessageId);
            const latest = [...(transcript?.messages ?? [])].reverse().find(message => message.role === "assistant");
            const currentPromptId = transcript?.pendingSendNowPromptId ?? active?.promptId ?? latest?.promptId;
            if (e.promptId && currentPromptId && e.promptId !== currentPromptId) return;
            if (findSessionSummary(e.sessionId)) {
              sessionsStore.getState().upsert({ sessionId: e.sessionId, status: "failed" });
            }
            // Only surface for the focused session — background sessions
            // finalizing after a switch shouldn't hijack the error banner.
            const currentSessionId = sessionStore.getState().sessionId;
            if (e.sessionId !== currentSessionId) {
              return;
            }
            const msg = e.detail
              ? friendlyError(e.detail)
              : e.kind === "rate_limit"
                ? "模型请求受限，请检查服务的调用频率或可用额度后再试。"
                : "本轮执行出错，已有内容已保留。";
            sessionStore.getState().setError(msg, e.promptId ? { sessionId: e.sessionId, promptId: e.promptId } : undefined);
            reportEvent("turn_error", "error", { sessionId: e.sessionId, kind: e.kind });
          },
        });
        if (disposed) {
          stopListening();
          return;
        }
        unlisten = stopListening;

        if (runtimeRecoveryRequested.current) {
          const sessionId = sessionsStore.getState().currentSessionId;
          const entry = sessionId ? findSessionSummary(sessionId) : undefined;
          if (entry && !sessionsStore.getState().deletedSessionIds.includes(entry.sessionId)) {
            // A failed first replay may have populated only part of history.
            // Refetch that projection on retry instead of suppressing it as a
            // complete cached transcript. All input remains in durable drafts.
            if (rendererHistoryPending.current) sessionStore.getState().dropSessionCache(entry.sessionId);
            sessionStore.getState().setSession(entry.sessionId);
            let loadedModel: string | null;
            let historyLoaded = false;
            try {
              loadedModel = await agentLoadSession(entry.sessionId, entry.cwd);
              historyLoaded = true;
              rendererHistoryPending.current = false;
            } finally {
              const status = findSessionSummary(entry.sessionId)?.status;
              // A host crash can leave history without a terminal event. A
              // healthy Runtime may still own a live turn during UI recovery.
              if (!isAgentOwnedActiveStatus(status)) {
                sessionStore.getState().finalizeIncompleteReplay(entry.sessionId, historyLoaded && status === "completed");
              }
              sessionStore.getState().clearReplaySuppression(entry.sessionId);
            }
            if (disposed) return;
            if (sessionsStore.getState().currentSessionId === entry.sessionId && !sessionsStore.getState().deletedSessionIds.includes(entry.sessionId)) {
              sessionsStore.getState().upsert({ sessionId: entry.sessionId, currentModelId: loadedModel ?? entry.currentModelId });
            }
          } else if (sessionId && !sessionsStore.getState().deletedSessionIds.includes(sessionId)) {
            throw new Error("无法确定当前会话的工作目录，请从任务列表重新打开会话后重试。");
          }
          runtimeRecoveryRequested.current = false;
          setInit(result);
          authReadyRef.current = result.auth.ready;
        }

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
    recoverySessionId,
    enqueueToast,
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
  const runtimeSetupHint = init?.auth.reason ?? "请先在「设置 → 模型」检查可用模型和连接";
  const chatSetupHint = sessionLoading
    ? "正在加载会话信息…"
    : modelSwitching
    ? "正在切换模型…"
    : models.length === 0
      ? "请先在「设置 → 模型」配置模型"
      : !activeSessionModelId
        ? currentEntry?.currentModelId
          ? `此会话使用的模型「${findModelDisplayName(models, currentEntry.currentModelId)}」当前不可用，请在右下角选择模型`
          : "未能获取此会话的模型信息，请重新打开任务，或在右下角选择模型"
        : runtimeSetupHint;

  const showActionToast = useCallback((message: string, actions: ToastAction[], durationMs = 8000, kind: ToastMessageKind = "auto") => {
    enqueueToast(message, actions, durationMs, kind);
  }, [enqueueToast]);

  useEffect(() => {
    let disposed = false;
    const stops: Array<() => void> = [];
    const exit = createExitPreparationController({
      preparing: setExitPreparing,
      failed: (message) => showToast(message, 8000),
      beforePrepare: async () => {
        await confirmAutomationEditorExit(() => confirmAutomationExit({
        title: "退出前舍弃自动化草稿？",
        description: "自动化任务还有未保存的输入。继续编辑会保留草稿；确认退出后，这些输入将被舍弃。",
        cancelLabel: "继续编辑",
        confirmLabel: "舍弃修改并退出",
        danger: true,
        }));
        try { meetingDrafts.ensureDurable(); }
        catch (error) {
          const discard = await confirmAutomationExit({
            title: "会议草稿尚未保存",
            description: `${String(error).replace(/^Error:\s*/, "")}。可以取消退出并重试保存，或退出且不保存本次校对和纪要修改。`,
            cancelLabel: "取消退出，继续编辑",
            confirmLabel: "不保存本次修改并退出",
            danger: true,
          });
          if (!discard) throw new Error("已取消退出，会议草稿仍保留在当前窗口");
          meetingDrafts.allowExitWithoutSaving();
        }
      },
      cancelPrepare: dismissAutomationExit,
    });
    void Promise.allSettled([
      listen<DesktopExitNotice>("desktop://prepare-exit", ({ payload }) => exit.prepare(payload)),
      listen<DesktopExitFailure>("desktop://exit-failed", ({ payload }) => exit.fail(payload)),
      listen("desktop://runtime-restart-required", () => {
        if (!disposed) {
          runtimeRecoveryRequested.current = true;
          authReadyRef.current = false;
          sessionStore.getState().failAllStreaming("error", "更新未完成，本地 Agent 正在恢复，请稍候重试。");
          useMessageQueueStore.getState().retryAllSending();
          permissionStore.getState().clearAll();
          questionStore.getState().clearAll();
          for (const entry of sessionsStore.getState().independent) {
            if (isAgentOwnedActiveStatus(entry.status)) sessionsStore.getState().upsert({ sessionId: entry.sessionId, status: "failed" });
          }
          setInit(null);
          setInitAttempt((value) => value + 1);
        }
      }),
    ]).then(async (results) => {
      for (const result of results) if (result.status === "fulfilled") {
        if (disposed) result.value(); else stops.push(result.value);
      }
      if (disposed) return;
      if (results.some((result) => result.status === "rejected")) {
        showToast("退出保护监听未能完整建立；录音保存失败时应用会保留，请重新启动后重试。", 8000);
        return;
      }
      try {
        const pending = await invoke<DesktopExitNotice | null>("desktop_exit_listener_ready");
        if (!disposed && pending) exit.prepare(pending, true);
      } catch (error) {
        if (!disposed) showToast(`退出保护状态确认失败：${friendlyError(error)}`, 8000);
      }
    });
    return () => { disposed = true; exit.dispose(); stops.forEach((stop) => stop()); };
  }, [showToast, confirmAutomationExit, dismissAutomationExit]);

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
    showToast(`所选模型“${findModelDisplayName(models, requestedModelId)}”已被删除或停用，请重新选择`, 5000);
    return undefined;
  };
  const navigateNow = (label: string) => {
    setNotificationAutomationOpen(null);
    if (label !== "专家·技能·连接器") setCreateExpertRequested(false);
    if (label === "个人记忆" || label === "资料库" || label === "更多") {
      openSettings("memory");
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
    // Extension management uses the active Runtime session's trusted workspace
    // and broadcasts changes from it. Clearing that session on entry made all
    // install/source/toggle actions unreachable, even after opening a chat.
    if (isCapabilityView(label)) {
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
  const handleNavigate = (label: string) => {
    if (label === "已归档") { openSettings("archived"); return; }
    navigateWithGuard(label);
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
    showToast(`用量已达到 ${quota.limit.toLocaleString()} 的上限，请到「用量统计」调整`);
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
    let cwd = newSessionTargetCwd;
    const pendingExpert = usePendingExpertStore.getState().expert;
    let createdSessionId: string | null = null;
    setCreatingSession(true);
    setHomeSendError(null);
    try {
      const permissionState = usePermissionModeStore.getState();
      const draftPermissionMode = permissionState.capabilityStatus?.locked
        ? permissionState.capabilityStatus.permissionMode
        : permissionState.homeMode;
      if (automaticWorkspace) cwd = await agentPrepareTaskWorkspace(taskWorkspaceRequestRef.current);
      const sessionId = await agentNewSession(cwd, modelId, draftPermissionMode);
      taskWorkspaceRequestRef.current = crypto.randomUUID();
      createdSessionId = sessionId;
      if (requestedMode !== "default") {
        await setCodingMode(sessionId, requestedMode);
      }
      if (pendingExpert) {
        const saved = await agentSetSessionExpert(sessionId, pendingExpert.expertId, pendingExpert.name, "local", pendingExpert.avatarLocal, pendingExpert.definitionPath, pendingExpert.definitionRaw);
        if (!saved) throw new Error("专家身份未能保存，请重试；本次消息尚未发送");
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
      const pending = pendingExpert;
      let textForAgent = text;
      if (pending && pending.prompt) {
        // Wrap persona in hidden markers. EchoAgent sees it as instructions;
        // MessageItem strips it from display on history replay.
        textForAgent = `${EXPERT_PERSONA_BEGIN}\n${pending.prompt}\n${EXPERT_PERSONA_END}\n\n${text}`;
        // Bind expert to session for persistence + UI badge.
        sessionsStore.getState().upsert({ sessionId, expertId: pending.expertId, expertName: pending.name, expertAvatar: pending.avatarLocal });
      }

      const accepted = beginAgentTurn({
        sessionId,
        promptText: textForAgent,
        displayText: text,
        attachments,
      });
      if (!accepted) return false;
      if (pending && usePendingExpertStore.getState().expert === pending) usePendingExpertStore.getState().clear();
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
    coding?: CodingTurnOptions,
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
      if (project && (isFirstUserTurn || projectConversation?.pendingProjectContext)) {
        const boundCwd = findSessionSummary(sessionId)?.cwd;
        if (!boundCwd) throw new Error("无法确认本会话的原工作目录，请重新打开任务");
        await agentsValidateReferences(boundCwd, project.experts);
      }
      const promptBody = promptTextOverride ?? sendText;
      const { promptText: textForAgent, consumedPendingContext } = projectPromptForTurn(
        project, projectConversation, isFirstUserTurn, promptBody,
      );
      const accepted = beginAgentTurn({
        sessionId,
        promptText: textForAgent,
        displayText: sendText,
        attachments: sendAttachments,
        coding,
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
      if (!summary?.cwd) throw new Error("无法确认原工作目录，请重新打开任务；原回复已保留");
      await agentPrepareSend(sessionId, summary.cwd);
      return true;
    } catch (error) {
      setTaskRefreshSignal(value => value + 1);
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
    coding: CodingTurnOptions = { mode: "agent", documentToolBridge: true },
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
      // Persist the initial user-selected session intent; following turns pass
      // their explicit mode without creating a different conversation.
      await setCodingMode(sessionId, coding.mode ?? "agent");
      const promptText = promptTextOverride ?? buildCodingWorkflowPrompt(requirement, contextPaths);
      const accepted = beginAgentTurn({
        sessionId,
        promptText,
        displayText: requirement,
        coding,
        attachments: coding.attachments,
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
    const codingTask = useTaskStore.getState().task;
    const coding: CodingTurnOptions | undefined = codingTask?.sessionId === sessionId ? {
      mode: transcript.agentMode === "ask" || transcript.agentMode === "plan" ? transcript.agentMode : "agent",
      documentToolBridge:true,workspace:useTaskStore.getState().root,taskId:codingTask.id,
    } : undefined;
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
      coding,
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
      if (latest.sessionId === sessionId) latest.setError(detail, { sessionId, promptId });
      const stillWorking = useSessionStore.getState().transcripts[sessionId];
      if (!stillWorking?.streamingMessageId && !stillWorking?.pendingSendNowPromptId) {
        useMessageQueueStore.getState().pauseQueued(sessionId);
      }
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
    if (!beforeCancel?.streamingMessageId && !beforeCancel?.pendingSendNowPromptId
      && sessionStore.getState().agentMode !== "computer_use"
      && sessionStore.getState().agentMode !== "browser_use") {
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
      useMessageQueueStore.getState().pauseQueued(sessionId);
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
    if (newSessionPendingRef.current) return;
    setAutomaticWorkspace(!newCwd);
    taskWorkspaceRequestRef.current = crypto.randomUUID();
    setHomeSendError(null);
    setNewSessionTargetCwd(newCwd || init?.cwd || "");
    if (!newCwd) return;
    const current = sessionsStore.getState().workspaces;
    if (current.some((workspace) => workspace.cwd === newCwd)) return;
    const next = [{ cwd: newCwd, sessionCount: 0 }, ...current];
    sessionsStore.getState().setWorkspaces(next);
    setWorkspaces(next);
  };

  const handleSelectCodingWorkspace = (newCwd: string) => {
    // The first IDE may use the active/default cwd without having an entry in
    // recent Coding projects yet. Keep that displayed source when switching
    // to a new worktree so it remains reachable and survives restart.
    const previousCwd = codingCwd;
    setCodingWorkspaceCwd(newCwd);
    // Register the cwd in recent Coding projects and activate it.
    if (newCwd) {
      setCodingWorkspaces((current) => {
        const retained = previousCwd && !current.some((workspace) => workspace.cwd === previousCwd)
          ? [...current, workspaces.find((workspace) => workspace.cwd === previousCwd) ?? { cwd: previousCwd, sessionCount: 0 }]
          : current;
        return retained.some((workspace) => workspace.cwd === newCwd)
          ? retained
          : [{ cwd: newCwd, sessionCount: 0 }, ...retained];
      });
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
    for (const key of Object.keys(useMessageQueueStore.getState().queues)) {
      if (codingQueueOwner(key)?.sessionId === sessionId) useMessageQueueStore.getState().clear(key);
    }
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
    const title = findSessionSummary(sessionId)?.title || "未命名对话";
    showActionToast(`已归档“${title}”`, [
      {
        label: "撤销",
        onClick: async () => {
          try {
            const restored = await agentSetSessionArchived(sessionId, false);
            syncArchivedSessionState(sessionId, restored);
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
  };

  const handleArchiveSession = async (sessionId: string, archived: boolean, _explicitCwd?: string) => {
    const next = await agentSetSessionArchived(sessionId, archived);
    syncArchivedSessionState(sessionId, next);
    if (next) showArchivedSessionActions(sessionId);
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
      showActionToast(`对话已删除，部分记忆未能清理，请到记忆页面检查。\n${result.memoryCleanupWarning}`, [
        { label: "查看记忆", onClick: () => openSettings("memory") },
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
        openSettings("general");
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
    const generation = ++selectionGenerationRef.current;
    if (!intent && !preservePlaceholder) {
      let ownerId: string | null;
      try {
        ownerId = await resolveGroupForSession(sessionId, useGroupStore.getState().groups);
      } catch (error) {
        if (selectionGenerationRef.current !== generation) return;
        showToast(`暂时无法确认会话归属，请稍后重试：${friendlyError(error)}`, 6000);
        if (strict) throw error;
        return;
      }
      if (selectionGenerationRef.current !== generation) return;
      if (ownerId) {
        await useGroupStore.getState().select(ownerId);
        if (selectionGenerationRef.current === generation) handleNavigate("专家团");
        return;
      }
    }
    if (!preservePlaceholder && placeholderView === "代码开发" && codingLeaveGuardRef.current) {
      if (!(await codingLeaveGuardRef.current())) {
        if (strict) throw new Error("请先处理代码开发中的未保存内容");
        return;
      }
    }
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
    let codingOwner: { root: string; taskId: string } | null = null;
    if (!intent && entry.cwd) {
      const detail = await codingApi.taskForSession(entry.cwd,sessionId).catch(() => null);
      if (detail) codingOwner = {root:entry.cwd,taskId:detail.id};
      if (selectionGenerationRef.current !== generation) return;
    }
    if (codingOwner) {
      setCodingWorkspaceCwd(codingOwner.root);
      setActiveCodingWorkspaceCwd(codingOwner.root);
      setCodingWorkspaces(current => current.some(item => item.cwd === codingOwner!.root)
        ? current : [{ cwd: codingOwner!.root, sessionCount: 0 }, ...current]);
      setPlaceholderView("代码开发");
      useTaskStore.getState().setRoot(codingOwner.root);
      await useTaskStore.getState().selectTask(codingOwner.taskId);
      if (selectionGenerationRef.current !== generation) return;
    } else if (!preservePlaceholder) setPlaceholderView(null);
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
                if (closed) showToast("这项请求已处理", 5000);
                else if (!pending) showToast("正在加载待确认的操作", 5000);
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
  const handleForked = async (newId: string, sourceSessionId: string, sourceCwd?: string) => {
    const source = findSessionSummary(sourceSessionId);
    const cwd = source?.cwd ?? sourceCwd;
    if (!cwd) {
      showToast("分叉已创建，但缺少工作区信息，请刷新会话列表");
      void refreshSessionCatalog();
      return;
    }
    const modelId = resolveSessionModelId(models, source?.currentModelId);
    const fork: SessionSummary = {
      sessionId: newId,
      title: source?.title || "分叉会话",
      cwd,
      currentModelId: modelId,
      permissionMode: source?.permissionMode ?? "ask",
    };
    // The Runtime fork already exists. A failed project save must retain it
    // as a recoverable task rather than report that creating the fork failed.
    sessionsStore.getState().upsert(fork);
    try {
      await useProjectsStore.getState().associateForkWithSourceProject(sourceSessionId, fork);
    } catch (error) {
      const linked = useProjectsStore.getState().projects.some((project) =>
        project.conversations.some((conversation) => conversation.sessionId === newId),
      );
      showToast(linked
        ? `分叉已创建并保留，项目关联尚未确认保存。请在项目页重试保存：${friendlyError(error)}`
        : `分叉已创建并保留为独立会话，关联项目失败。可在会话菜单“移入项目…”重试：${friendlyError(error)}`, 7000);
    }
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
          "": "general",
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
      case "usage":
        handleNavigate("用量统计");
        return true;
      case "flush":
      case "dream": {
        if (args) {
          showToast(`用法：/${name}`);
          return false;
        }
        const sessionId = sessionsStore.getState().currentSessionId;
        if (placeholderView || !sessionId || sessionStore.getState().sessionId !== sessionId) {
          showToast(`/${name} 需要先打开任务会话`);
          return false;
        }
        if (memoryCommandsInFlightRef.current.has(sessionId)) {
          showToast("当前任务正在处理记忆，请稍候");
          return false;
        }
        const session = findSessionSummary(sessionId);
        const actions: ToastAction[] = [{
          label: "查看记忆",
          onClick: async () => {
            if (sessionsStore.getState().deletedSessionIds.includes(sessionId)) {
              throw new Error("原任务已删除，请在设置中查看保留的记忆");
            }
            if (sessionsStore.getState().currentSessionId !== sessionId) {
              await handleSelectSession(sessionId, session?.cwd, false, true);
            }
            if (sessionsStore.getState().currentSessionId !== sessionId) {
              throw new Error("任务打开已被新的导航操作取代，请重新打开记忆");
            }
            openSettings("memory");
          },
        }];
        const notify = (message: string, kind: ToastMessageKind = "info") => {
          const background = sessionsStore.getState().currentSessionId !== sessionId;
          const title = session?.title || "未命名对话";
          const shortTitle = title.length > 24 ? `${title.slice(0, 24)}…` : title;
          // Classify diagnostics before adding a user-owned task title. Keep
          // the originating task visible even while technical details collapse.
          const presentation = toastMessage(message, kind);
          const context = background ? `（任务：${shortTitle}）` : "";
          const details = [presentation.details, background && title !== shortTitle ? `任务：${title}` : ""].filter(Boolean).join("\n");
          showActionToast(`${presentation.summary}${context}${details ? `\n${details}` : ""}`, actions, 8000, "info");
        };
        memoryCommandsInFlightRef.current.add(sessionId);
        const progressId = enqueueToast(name === "flush" ? "正在提取记忆，完成后会提示结果" : "正在整理长期记忆，完成后会提示结果", [], 4000, "info", `memory:${sessionId}`);
        try {
          // Invoke the same Runtime maintenance path as the memory page. This
          // returns errors and completion even when no notification is emitted
          // (empty input, a busy processor, or a failed model request).
          const completed = name === "flush" ? await memoryFlush(sessionId) : await memoryDream(sessionId);
          notify(name === "flush" ? memoryFlushMessage(completed) : memoryDreamMessage(completed));
          return name === "dream" || completed;
        } catch (error) {
          notify(`${name === "flush" ? "记忆提取" : "记忆整理"}失败：${friendlyError(error)}`, "error");
          return false;
        } finally {
          if (progressId !== undefined) dismissToast(progressId);
          memoryCommandsInFlightRef.current.delete(sessionId);
        }
      }
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
        await memoryRecordSave({ scope: remember.scope, kind: remember.scope === "global" ? "user" : "project", key: remember.content.slice(0, 80), content: remember.content, pinned: true, validUntil: null }, memoryCwd || undefined);
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
      definitionPath: agent.path,
      definitionRaw: agent.raw,
      source: "local",
    });
    handleGoHome();
  };

  const createProjectSession = async (project: ProjectMeta, modelId: string) => {
    const requests = projectSessionRequestsRef.current;
    const previous = requests.get(project.id);
    if (previous?.pending) {
      showToast("正在创建项目对话，请稍候");
      return undefined;
    }
    const context = JSON.stringify([project.cwd || null, modelId]);
    const request = previous?.context === context
      ? previous : { context, requestId: crypto.randomUUID(), pending: false };
    request.pending = true;
    requests.set(project.id, request);
    try {
      const cwd = project.cwd || await agentPrepareTaskWorkspace(request.requestId);
      await agentsValidateReferences(cwd, project.experts);
      const sessionId = await agentNewSession(cwd, modelId);
      requests.delete(project.id);
      return { sessionId, cwd };
    } finally {
      request.pending = false;
    }
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
      const created = await createProjectSession(project, modelId);
      if (!created) return;
      const { sessionId, cwd } = created;
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
      const created = await createProjectSession(project, modelId);
      if (!created) return undefined;
      const { sessionId, cwd } = created;
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
    onSendCodingMessage: (value: string, promptTextOverride?: string, coding?: CodingTurnOptions) =>
      handleSendCurrent(value, coding?.attachments ?? [], undefined, promptTextOverride, coding),
    onCorrectCodingRun: (value: string) => handleSendNowCurrent(value),
    onCancelCodingRun: () => handleCancel("stop"),
    onStartProject: handleStartProject,
    onStartProjectConversation: handleStartProjectConversation,
    onRenameSession: handleRenameSession,
    onArchiveSession: handleArchiveSession,
    onDeleteSession: handleDeleteSession,
    automationRefreshSignal,
  };

  const modelCatalogNotice = init?.ok && modelCatalogError ? (
    <div className="app__notice app__notice--err" role="alert">
      模型列表加载失败：{modelCatalogError}
      <button type="button" className="btn btn--ghost" onClick={() => void refreshModels()}>重试</button>
    </div>
  ) : null;

  return (
    <div className={`app${IS_MACOS ? " app--macos" : ""}${codingWorkspaceActive ? " app--coding" : ""}`}>
      {exitPreparing && !automationExitDialog && <div className="app__exit-overlay" role="status" aria-live="polite">正在保存录音与任务数据并准备退出…</div>}
      {init?.ok && <CodingTaskRuntimeManager apiReady={init.auth.ready} availableModelIds={models.map(model => model.id)} roots={[...new Set([
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
          onOpenSettings={() => openSettings("general")}
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
          {(!currentSessionId || placeholderView) && !codingWorkspaceActive && (
            <header className="main-page-controls" data-tauri-drag-region>
              <SessionNavigation sidebarCollapsed={sidebarCollapsed} onToggleSidebar={() => setSidebarCollapsed(!sidebarCollapsed)} onNewTask={handleNewSession} />
            </header>
          )}
          {(!currentSessionId || placeholderView) && modelCatalogNotice}
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
              {runtimeAutoRetry !== null ? `AI 引擎将在稍后自动恢复（${runtimeAutoRetry}/${MAX_RECOVERY_ATTEMPTS}）：` : runtimeRecoveryRequested.current && runtimeRecoveryAttempts.current.length >= MAX_RECOVERY_ATTEMPTS ? "自动恢复已暂停，请手动重试：" : "初始化未完成："}{initError}
              <br />
              <button
                type="button"
                className="btn btn--primary"
                onClick={() => {
                  runtimeRecoveryAttempts.current = [];
                  runtimeRecoverySuppressed.current = false;
                  setInit(null);
                  setInitError(null);
                  setInitAttempt((attempt) => attempt + 1);
                }}
              >
                重试初始化
              </button>
              {runtimeAutoRetry !== null && <button type="button" className="btn" onClick={() => {
                runtimeRecoverySuppressed.current = true;
                setRuntimeAutoRetry(null);
                setInitError((error) => `${error ?? "AI 引擎暂不可用"} 自动恢复已停止，可手动重试。`);
              }}>停止自动恢复</button>}
            </div>
          ) : !init ? (
            <div className="app__notice">正在准备任务执行环境…</div>
          ) : !init.ok ? (
            <div className="app__notice app__notice--err">
              EchoAgent 尚未就绪：{init.auth.reason ?? "未知原因"}
              <br />
              请在「设置 → 模型」检查可用模型和连接。
            </div>
          ) : codingWorkspaceActive ? null : (
            <Suspense fallback={<div className="app__notice" role="status">正在加载界面…</div>}>
              {placeholderView === "专家团" ? (
                <CapabilitySection active="专家团" onNavigate={handleNavigate}>
                  <GroupWorkspace cwd={newSessionTargetCwd} modelId={newSessionModelId} models={models} workspaces={workspaces} onToast={showToast} onOpenSettings={() => openSettings("model")} onOpenKnowledgeBase={() => handleNavigate("知识库")} onOpenOrganization={() => handleNavigate("组织")} />
                </CapabilitySection>
              ) : placeholderView ? (
                <PlaceholderPage
                  {...placeholderProps}
                  label={placeholderView}
                  cwd={newSessionTargetCwd}
                  onSelectWorkspace={handleSelectWorkspace}
                />
              ) : currentSessionId ? (
                <ChatView
                  taskRefreshSignal={taskRefreshSignal}
                  onOpenTeamGroup={team => {
                    if (!currentSessionId) return;
                    const referenceText = sessionStore.getState().messages.slice(-12).map(message => `${message.role === "user" ? "用户" : "助手"}：${message.parts.filter(part => part.kind === "text").map(part => part.kind === "text" ? part.text : "").join("")}`).join("\n\n").slice(-30000);
                    useGroupStore.getState().fromTeam(currentSessionId, team, referenceText);
                    handleNavigate("专家团");
                  }}
                  title={currentTitle}
                  notice={modelCatalogNotice}
                  onRename={handleRenameTitle}
                  navigation={{ sidebarCollapsed, onToggleSidebar: () => setSidebarCollapsed(!sidebarCollapsed), onNewTask: handleNewSession }}
                  headerLeading={activeSubagentParent ? <button type="button" className="main-topbar__return"
                    onClick={handleReturnToParentSession} aria-label={`返回上级任务：${parentSessionTitle}`} title={`返回上级任务：${parentSessionTitle}`}>
                    <ChevronLeftIcon size="sm" /><span>返回上级任务</span>
                  </button> : undefined}
                  onSend={handleSendCurrent}
                  onSendNow={handleSendNowCurrent}
                  onCancel={handleCancel}
                  cancelling={cancellingSessionId === currentSessionId}
                  apiReady={chatReady}
                  setupHint={chatSetupHint}
                  onOpenSettings={() => openSettings("model")}
                  onOpenSecuritySettings={() => openSettings("security")}
                  modelId={activeSessionModelId}
                  modelLoading={sessionLoading || modelSwitching}
                  models={models}
                  onModelChange={handleModelChange}
                  cwd={activeSessionCwd}
                  onNewTask={handleNewSession}
                  onReloadWorkspace={async (originalCwd) => {
                    const previousError = sessionStore.getState().error;
                    sessionsStore.getState().upsert({sessionId: currentSessionId, cwd: originalCwd});
                    await handleSelectSession(currentSessionId, originalCwd, true, true);
                    if (sessionStore.getState().sessionId === currentSessionId && sessionStore.getState().error === previousError) {
                      sessionStore.getState().setError(null);
                    }
                  }}
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
                  cwd={automaticWorkspace ? undefined : newSessionTargetCwd}
                  automaticWorkspace={automaticWorkspace}
                  defaultWorkspaceCwd={init.cwd}
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
      <div className="toast-stack" aria-label="操作提示">
        {toasts.map((toast) => (
          <Toast
            key={toast.id}
            message={toast.message}
            actions={toast.actions}
            kind={toast.kind}
            stacked
            onDismiss={() => dismissToast(toast.id)}
            onPause={(paused) => pauseToast(toast.id, paused)}
            onActionError={(error) => showToast(`操作未完成：${String(error)}`, undefined, "error")}
          />
        ))}
      </div>
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
      <GroupActionsHost onFeedback={showActionToast} />
      {automationExitDialog}
    </div>
  );
}
