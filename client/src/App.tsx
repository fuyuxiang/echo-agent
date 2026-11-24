import { bindTeamProjectSession, canEditTeamProject, captureTeamProjectOperation, projectRoleUnavailable, teamProjectRequest, type OpenTeamExecution, type TeamProjectSnapshot, type StartTeamWork } from "@/lib/team-project-client";
import { isPrivateMemoryLookup, privateMemoryInSharedProject } from "@/lib/private-memory-lookup";
import { documentEditPrompt, forgetDocumentSessions } from "@/lib/document-workspace";
import { findModelDisplayName } from "@/lib/model-presentation";
import { SessionNavigation } from "./components/SessionHeader";
import { PageChromeProvider } from "./components/PageToolbar";
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
import {
  CAPABILITY_SECTION_LABEL, ComposerCapabilityNavigation, isCapabilityView,
  type ComposerCapabilityDestination, type OpenComposerCapability,
} from "./lib/capability-navigation";
import { Toast, type ToastAction } from "./components/Toast";
import { useToastQueue } from "./lib/use-toast-queue";
import { useRendererRecoveryStatus, type RendererRecoveryStatus } from "./lib/renderer-recovery";
import { readRecoverySessionId, saveRecoverySessionId } from "./lib/recovery-context";
import { legacyWorkspaceSessions } from "./lib/legacy-workspace";
import { planRecoveryAttempt, MAX_RECOVERY_ATTEMPTS } from "./lib/recovery-policy";
import { toastMessage, type ToastMessageKind } from "./lib/toast-message";
import { memoryDreamMessage, memoryFlushMessage } from "./lib/memory-feedback";
import { memoryTitleFromContent } from "./lib/memory-title";
// PermissionDialog is now inline in ChatView (PermissionInlineCard), not a global modal.
import { ThemeProvider } from "./components/ThemeProvider";
import type { SettingsDirectTarget, SettingsSectionId } from "./components/SettingsPanel";

import { ChevronLeftIcon } from "./foundation/components/Icon/icons";
import type { ModelOption } from "./components/ModelSelector";
import { ModelCatalogContext, ModelManagementContext } from "./lib/model-management";
import {
  isConfiguredModelId,
  resolveConfiguredModelId,
  resolveSessionModelId,
} from "./lib/model-selection";
import { sessionControlReceiptState, useSessionStore } from "./stores/session-store";
import { useSessionsStore } from "./stores/sessions-store";
import { useContinuationStore } from "./stores/continuation-store";
import { latestContinuationTarget } from "./lib/session-continuation";
import { useDesktopTray } from "./lib/use-desktop-tray";
import { TrayActionCancelled } from "./lib/desktop-tray-connection";
import { reconcileSessionCatalog } from "./lib/session-catalog";
import { pendingSessionInteractionStatus, restoreSessionInteractions } from "./lib/session-interactions";
import { meetingDrafts } from "./lib/meeting-drafts";
import { createExitPreparationController, type DesktopExitNotice, type DesktopExitFailure, type ExitPreparationStage } from "./lib/recording-exit";
import { usePermissionStore } from "./stores/permission-store";
import {
  hasPendingQuestionForSession,
  useQuestionStore,
} from "./stores/question-store";
import { usePendingExpertStore } from "./stores/pending-expert-store";

import {
  agentInit,
  agentNewSession,
  agentSessionInfo,
  type PermissionMode,
  agentPrepareTaskWorkspace,
  agentPrepareSend,
  agentSendNow,
  agentCancel,
  agentLoadSession,
  agentRecoverSession,
  agentReloadSession,
  agentSessionLoadInFlight,
  agentListSessionHistory,
  agentFindSessionSummary,
  agentListWorkspaces,
  agentRenameSession,
  agentDeleteSession,
  agentSetSessionArchived,
  agentSetModel,
  agentSetSessionExpert,
  agentAuthStatus,
  sessionContinue,
  type ContinuationRequest,
  setAgentMode,
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
import type { AgentEntry, SessionSummary, ContinuationTarget, ConversationNavigationRequest } from "./lib/types";
import { flushProjectPersistence, hydrateProjectsFromBackend, projectOwnsSession, useProjectsStore, type ProjectMeta, type ProjectTab } from "./stores/projects-store";
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
import { readTurnIdentity } from "./lib/execution-turn";
import { groupCreate, groupDelete, groupList, groupSend, resolveGroupForSession } from "./features/groups/group-client";
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
import { friendlyError, isTaskWorkspaceError } from "./lib/error-format";
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
import { buildProjectPrompt, projectSessionTitle, type ProjectConversationOptions, type StartProjectConversation } from "./lib/project-context";
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
  readPersistedSessionControl,
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
  | { kind: "return"; point: SubagentReturnPoint }
  | { kind: "project-work-item"; projectId: string };

class SessionNavigationSupersededError extends Error {
  constructor() {
    super("任务打开已被新的导航操作取代");
    this.name = "SessionNavigationSupersededError";
  }
}

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

/** Find a loaded summary without depending on a render. */
function findSessionSummary(sessionId: string): SessionSummary | undefined {
  return useSessionsStore.getState().independent
    .find((entry) => entry.sessionId === sessionId);
}

export default function App({ uiRestoreDeferred = false }: { uiRestoreDeferred?: boolean } = {}) {
  return (
    <ThemeProvider>
      <Shell uiRestoreDeferred={uiRestoreDeferred} />
    </ThemeProvider>
  );
}

function prepareExistingProjectTurn(sessionId: string, body: string) {
  const project = useProjectsStore.getState().projects.find((item) =>
    projectOwnsSession(item, sessionId),
  );
  const conversation = project?.conversations.find((item) => item.sessionId === sessionId);
  const isFirstUserTurn = !useSessionStore.getState().transcripts[sessionId]?.messages.some(
    (message) => message.role === "user",
  );
  const consumedPendingContext = !!conversation?.pendingProjectContext;
  const appliesContext = Boolean(project && conversation && (isFirstUserTurn || consumedPendingContext));
  const prepared = {
    promptText: body,
    setPending: (pending: boolean) => {
      if (project && (pending ? appliesContext : consumedPendingContext)) {
        useProjectsStore.getState().setProjectContextPending(project.id, sessionId, pending);
      }
    },
  };
  const summary = findSessionSummary(sessionId);
  if (!project && !summary?.projectId) return prepared;
  return (async () => {
    if (project && appliesContext) {
      const cwd = summary?.cwd;
      if (!cwd) throw new Error("无法确认本会话的原工作目录，请重新打开任务");
      await agentsValidateReferences(cwd, project.experts);
      const current = useProjectsStore.getState().projects.find(item => projectOwnsSession(item, sessionId));
      if (current?.id !== project.id || current.experts !== project.experts || findSessionSummary(sessionId)?.cwd !== cwd) {
        throw new Error("项目专家或工作目录已变更，请重新发送");
      }
    }
    await flushProjectPersistence();
    return prepared;
  })();
}

function Shell({ uiRestoreDeferred }: { uiRestoreDeferred: boolean }) {
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
  const groupChatOpen = useGroupStore(state => !!state.current);
  const [initAttempt, setInitAttempt] = useState(0);
  const [exitStage, setExitStage] = useState<ExitPreparationStage | null>(null);
  const exitPreparing = exitStage !== null;
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
  const [settingsModelId, setSettingsModelId] = useState<string>();
  const [settingsTarget, setSettingsTarget] = useState<SettingsDirectTarget>();
  const [searchOpen, setSearchOpen] = useState(false);
  const [trayAttention, setTrayAttention] = useState<{ sessionId: string; sequence: number } | null>(null);
  const [trayNewRequest, setTrayNewRequest] = useState(0);
  const [aboutOpen, setAboutOpen] = useState(false);
  const [updateDialogOpen, setUpdateDialogOpen] = useState(false);
  const [trustRequest, setTrustRequest] = useState<{ cwd?: string; reason?: string } | null>(null);
  const [taskRefreshSignal, setTaskRefreshSignal] = useState(0);
  const [automationRefreshSignal, setAutomationRefreshSignal] = useState(0);
  const [notificationAutomationOpen, setNotificationAutomationOpen] = useState<{ id: string; sequence: number } | null>(null);
  const notificationAutomationSequenceRef = useRef(0);
  const automationAdmissionsRef = useRef(new Set<string>());
  const automationSessionRunsRef = useRef(new Map<string, { recordId: string; promptId?: string }>());
  const [commandRefreshKey, setCommandRefreshKey] = useState(0);
  const [placeholderView, setPlaceholderView] = useState<string | null>(null);
  const [capabilityVisit, setCapabilityVisit] = useState<{
    destination: ComposerCapabilityDestination;
    cwd?: string;
  } | null>(null);
  const capabilityReturnFocus = useRef<HTMLElement | null>(null);
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
  const [sessionRestore, setSessionRestore] = useState<{ sessionId: string; generation: number; error?: string } | null>(null);
  const [subagentTrail, setSubagentTrail] = useState<SubagentReturnPoint[]>([]);
  const [projectWorkItemSource, setProjectWorkItemSource] = useState<{ projectId: string; sessionId: string } | null>(null);
  const [subagentScrollRestore, setSubagentScrollRestore] = useState<SubagentScrollRestore | null>(null);
  const subagentRestoreSequenceRef = useRef(0);
  const [conversationNavigation, setConversationNavigation] = useState<ConversationNavigationRequest | null>(null);
  const conversationNavigationSequence = useRef(0);
  const slashContinuationRequests = useRef(new Map<string, ContinuationRequest>());
  const [creatingSession, setCreatingSession] = useState(false);
  const [newTaskMode, setNewTaskMode] = useState<AutomationMode>("default");
  const [homeSendError, setHomeSendError] = useState<string | null>(null);
  const [workspaces, setWorkspaces] = useState<WorkspaceInfo[]>([]);
  /** Workspace selected for the next session; independent of the active session cwd. */
  const [newSessionTargetCwd, setNewSessionTargetCwd] = useState("");
  const [automaticWorkspace, setAutomaticWorkspace] = useState(true);
  const taskWorkspaceRequestRef = useRef(crypto.randomUUID());
  const projectSessionRequestsRef = useRef(new Map<string, { context: string; requestId: string; pending: boolean }>());
  const preparingProjectTurns = useRef(new Set<string>());
  const [cancellingSessionId, setCancellingSessionId] = useState<string | null>(null);
  const modelsRef = useRef<ModelOption[]>([]);
  const authReadyRef = useRef(false);
  const promptedUpdateVersionRef = useRef<string | null>(null);
  const newSessionPendingRef = useRef(false);
  const selectionGenerationRef = useRef(0);
  const handleProjectNavigation = useCallback(() => { selectionGenerationRef.current += 1; }, []);
  const sessionCatalogGenerationRef = useRef(0);
  const historyRequestInFlight = useRef(false);
  const [historyCursor, setHistoryCursor] = useState<string | null>(null);
  const [archiveHistoryCursor, setArchiveHistoryCursor] = useState<string | null>(null);
  const [historyIncomplete, setHistoryIncomplete] = useState(false);
  const [loadingMoreHistory, setLoadingMoreHistory] = useState(false);
  const modelCatalogGenerationRef = useRef(0);
  const memoryCommandsInFlightRef = useRef(new Set<string>());
  const manuallyNamedSessionsRef = useRef(new Set<string>());

  const showToast = useCallback((message: string, durationMs?: number, kind: ToastMessageKind = "auto") => {
    enqueueToast(message, [], durationMs, kind);
  }, [enqueueToast]);

  const sessionStore = useSessionStore;
  const sessionsStore = useSessionsStore;
  const sessionCatalogLoading = useSessionsStore(state => state.loading);
  const permissionStore = usePermissionStore;
  const questionStore = useQuestionStore;
  const updateStatus = useUpdateStore((state) => state.status);
  const availableUpdateVersion = useUpdateStore((state) => state.update?.version);
  const modalOpen = useModalPresence();
  const { confirm: confirmAutomationExit, dismiss: dismissAutomationExit, dialog: automationExitDialog } = useAppDialog();

  const openSettings = useCallback((section: SettingsSectionId = "general", target?: SettingsDirectTarget) => {
    setSettingsModelId(undefined);
    setSettingsSection(section);
    setSettingsTarget(target);
    setSearchOpen(false);
    setAboutOpen(false);
    setSettingsOpen(true);
  }, []);
  useEffect(() => {
    if (uiRestoreDeferred) enqueueToast("部分界面数据已保存为恢复档案，可以继续使用应用", [
      { label: "查看恢复档案", onClick: () => openSettings("data", "ui-recovery") },
    ], 12000, "info");
  }, [enqueueToast, openSettings, uiRestoreDeferred]);
  const [legacySessions, setLegacySessions] = useState<ReadonlySet<string>>(() => new Set());
  useEffect(() => {
    if (!("__TAURI_INTERNALS__" in window)) return;
    let active = true;
    void legacyWorkspaceSessions().then(ids => {
      if (active && Array.isArray(ids)) setLegacySessions(new Set(ids));
    }).catch(() => {});
    return () => { active = false; };
  }, []);
  const openModelManagement = useCallback((modelId?: string) => {
    openSettings("model");
    setSettingsModelId(modelId);
  }, [openSettings]);
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
      .then((session) => session?.loggedIn
        ? Promise.all([internalReload("skills"), internalReload("models")])
        : undefined)
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
  const refreshSessionCatalog = useCallback(async (refresh = false) => {
    const store = sessionsStore.getState();
    if (store.loading) return;
    const generation = ++sessionCatalogGenerationRef.current;
    const catalogAtStart = store.independent;
    store.setLoading(true);
    store.setError(null);
    historyRequestInFlight.current = false;
    setLoadingMoreHistory(false);
    const [sessionResult, workspaceResult] = await Promise.allSettled([
      refresh ? agentListSessionHistory(undefined, true) : agentListSessionHistory(),
      agentListWorkspaces(),
    ]);
    if (sessionCatalogGenerationRef.current !== generation) return;
    const failures: string[] = [];
    if (sessionResult.status === "fulfilled") {
      const page = sessionResult.value;
      const current = sessionsStore.getState().independent;
      const reconciled = reconcileSessionCatalog(page.sessions, catalogAtStart, current);
      if (page.nextCursor || page.nextArchiveCursor || page.incomplete) {
        sessionsStore.getState().mergeSessions(reconciled);
      } else {
        // History pages omit hidden sessions. Keep summaries already opened
        // through a parent task or a project association.
        const included = new Set(reconciled.map(entry => entry.sessionId));
        sessionsStore.getState().setIndependent([
          ...reconciled,
          ...current.filter(entry => entry.hidden && !included.has(entry.sessionId)),
        ]);
      }
      setHistoryCursor(page.nextCursor);
      setArchiveHistoryCursor(page.nextArchiveCursor);
      setHistoryIncomplete(page.incomplete);
      useProjectsStore.getState().reconcileSessionArchiveStates(reconciled);
    } else {
      setHistoryIncomplete(true);
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

  const loadMoreSessionHistory = useCallback(async (archived: boolean) => {
    const cursor = archived ? archiveHistoryCursor : historyCursor;
    if (!cursor || historyRequestInFlight.current || sessionsStore.getState().loading) return;
    const generation = sessionCatalogGenerationRef.current;
    const before = sessionsStore.getState().independent;
    historyRequestInFlight.current = true;
    setLoadingMoreHistory(true);
    try {
      const page = await agentListSessionHistory(cursor);
      if (generation !== sessionCatalogGenerationRef.current) return;
      const store = sessionsStore.getState();
      const reconciled = reconcileSessionCatalog(page.sessions, before, store.independent);
      store.mergeSessions(reconciled);
      useProjectsStore.getState().reconcileSessionArchiveStates(reconciled);
      if (archived) setArchiveHistoryCursor(page.nextArchiveCursor);
      else setHistoryCursor(page.nextCursor);
      setHistoryIncomplete(current => current || page.incomplete);
    } catch (error) {
      if (generation === sessionCatalogGenerationRef.current) {
        setHistoryIncomplete(true);
        showToast(`加载历史任务失败：${friendlyError(error)}`, 6000);
      }
    } finally {
      if (generation === sessionCatalogGenerationRef.current) {
        historyRequestInFlight.current = false;
        setLoadingMoreHistory(false);
      }
    }
  }, [archiveHistoryCursor, historyCursor, sessionsStore, showToast]);

  const historyControls = (archived: boolean) => ({
    hasMore: (archived ? archiveHistoryCursor : historyCursor) !== null,
    incomplete: historyIncomplete,
    loading: loadingMoreHistory || sessionCatalogLoading,
    loadMore: () => void loadMoreSessionHistory(archived),
    refresh: () => void refreshSessionCatalog(true),
  });

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
        window.dispatchEvent(new Event("echo:model-catalog-changed"));
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
            const entry = await agentFindSessionSummary(recoverySessionId);
            if (disposed) return;
            if (selectionGenerationRef.current === selectionAtStart && !sessionsStore.getState().currentSessionId) {
              if (entry && !entry.archived && !sessionsStore.getState().deletedSessionIds.includes(entry.sessionId)) {
                sessionsStore.getState().mergeSessions([entry]);
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
            const pendingStatus = updateSessionId ? pendingSessionInteractionStatus(updateSessionId) : undefined;
            const currentStatus = updateSessionId
              ? findSessionSummary(updateSessionId)?.status
              : undefined;
            if (
              updateSessionId
              && applied && currentPromptUpdate && u._meta?.isReplay !== true
              && updatedTranscript?.streamingMessageId && !updatedTranscript.control && !pendingStatus
              && ["user_message_chunk", "agent_message_chunk", "agent_thought_chunk", "tool_call", "tool_call_update", "plan"].includes(updateType ?? "")
              && (currentStatus === "pending" || currentStatus === "completed" || isWaitingForUser(currentStatus))
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
                status: pendingSessionInteractionStatus(sessionId) ?? "working",
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
          onComplete: (p) => {
            if (sessionsStore.getState().deletedSessionIds.includes(p.sessionId)) return;
            const requestedAction = sessionStore.getState().transcripts[p.sessionId]?.control?.action;
            const terminalStatus = p.recoverySummary ? "stopped" : terminalSessionStatus(p, requestedAction);
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
            // Retain the legacy prompt identity before completion can discard
            // an empty assistant placeholder.
            const before = sessionStore.getState().transcripts[p.sessionId];
            const completedPromptId = p.promptId || before?.messages.find(
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
              if (completedPromptId) useMessageQueueStore.getState().settleSending(
                p.sessionId,
                queuePolicy.settlement,
                completedPromptId,
              );
            }
            // An old/duplicate terminal event may enrich its own message, but
            // cannot settle the replacement turn's UI or dispatch more work.
            if (!completedCurrentTurn || currentTranscript?.streamingMessageId
              || currentTranscript?.pendingSendNowPromptId) return;
            if (!queuePolicy.autoAdvance || requestedAction || p.recoverySummary) {
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
              || p.recoverySummary
              || !queuePolicy.autoAdvance
              || pendingSessionInteractionStatus(p.sessionId)
            ) return;
            // Internal/external notifications are dispatched by the Rust bridge
            // for every session (including background automation sessions).
            // 消息队列自动续发(对齐 EchoAgent message-queue):该会话若有 active
            // 队列项,取下一条继续发送,实现「回完一条自动发下一条」。
            const q = useMessageQueueStore.getState().getQueue(p.sessionId);
            if (hasActiveItems(q)) {
              if (preparingProjectTurns.current.has(p.sessionId)) {
                // The replacement may be rejected once it observes the completed
                // turn. Keep later messages behind it for an explicit resume.
                useMessageQueueStore.getState().pauseQueued(p.sessionId, "原回复已结束，后续消息已暂停，请按顺序恢复发送");
                return;
              }
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
              const next = useMessageQueueStore.getState().claimNext(p.sessionId, queuedPromptId);
              if (!next) return;

              preparingProjectTurns.current.add(p.sessionId);
              void (async () => {
                const retry = () => useMessageQueueStore.getState().settleSending(
                  p.sessionId, "retry", queuedPromptId,
                );
                try {
                  const body = next.documentEdit ? documentEditPrompt(next.documentEdit, next.text) : next.text;
                  const candidate = prepareExistingProjectTurn(p.sessionId, body);
                  const prepared = candidate instanceof Promise ? await candidate : candidate;
                  const latest = sessionStore.getState().transcripts[p.sessionId];
                  if (sessionsStore.getState().deletedSessionIds.includes(p.sessionId)
                    || latest?.streamingMessageId || latest?.pendingSendNowPromptId
                    || latest?.control || pendingSessionInteractionStatus(p.sessionId)) {
                    retry();
                    return;
                  }
                  beginAgentTurn({
                    sessionId: p.sessionId,
                    promptText: prepared.promptText,
                    displayText: next.text,
                    attachments: next.attachments,
                    promptId: queuedPromptId,
                    allowBackgroundSession: true,
                    onRejected: () => {
                      prepared.setPending(true);
                      retry();
                    },
                  });
                  prepared.setPending(false);
                } catch (error) {
                  retry();
                  useMessageQueueStore.getState().pauseQueued(p.sessionId);
                  if (sessionStore.getState().sessionId === p.sessionId) {
                    sessionStore.getState().setError(friendlyError(error));
                  }
                  if (findSessionSummary(p.sessionId)) {
                    sessionsStore.getState().upsert({ sessionId: p.sessionId, status: "failed" });
                  }
                } finally {
                  preparingProjectTurns.current.delete(p.sessionId);
                }
              })();
            }
          },
          onTurnUsage: (payload) => {
            const config = loadQuotaConfig();
            const session = findSessionSummary(payload.sessionId);
            const identity = readTurnIdentity({ turnOrigin: payload.turnOrigin }, payload.promptId);
            const next = recordTurnUsage(loadUsage(), {
              sessionId: payload.sessionId,
              promptId: payload.promptId,
              usage: payload.usage,
              occurredAt: payload.occurredAt,
              eventId: payload.eventId,
              fallbackModelId: session?.currentModelId,
              projectId: payload.projectId ?? undefined,
              turnOrigin: identity.origin,
              turnSourceId: identity.sourceId,
            }, config ?? undefined);
            publishQuotaAlert(next, config, payload.sessionId);
          },
          onSummary: ({ sessionId, title, isManual }) => {
            if (sessionsStore.getState().deletedSessionIds.includes(sessionId)) return;
            if (isManual) manuallyNamedSessionsRef.current.add(sessionId);
            else if (manuallyNamedSessionsRef.current.has(sessionId)) return;
            // Replace the provisional first-message title, or apply a manual
            // rename. Naming is metadata, not new conversation activity.
            sessionsStore.getState().upsert({
              sessionId,
              title,
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
            // Cleanup progresses after execution ended. It only invalidates
            // history; a delayed cleanup event must not settle a newer turn.
            if (["cleanup", "publication", "queued", "running"].includes(event.phase)) return;
            if (event.sessionId && event.cwd) {
              const sessionStore = sessionsStore.getState();
              if (sessionStore.deletedSessionIds.includes(event.sessionId)) return;
              const existing = sessionStore.independent.find(
                (session) => session.sessionId === event.sessionId,
              );
              const admission = JSON.stringify([event.recordId, event.promptId]);
              const admissions = automationAdmissionsRef.current;
              const newAdmission = event.phase === "sessionCreated" && Boolean(event.recordId) && !admissions.has(admission);
              if (event.phase === "sessionCreated" && !newAdmission) return;
              const admissionControl = useSessionStore.getState().transcripts[event.sessionId]?.control
                ?? readPersistedSessionControl(event.sessionId);
              const admitNewPrompt = newAdmission && Boolean(event.promptId)
                && admissionControl?.promptId !== event.promptId;
              if (newAdmission) {
                admissions.add(admission);
                if (admissions.size > 2000) admissions.delete(admissions.values().next().value!);
                automationSessionRunsRef.current.set(event.sessionId, { recordId: event.recordId, promptId: event.promptId });
                if (automationSessionRunsRef.current.size > 2000) automationSessionRunsRef.current.delete(automationSessionRunsRef.current.keys().next().value!);
                // Native dispatch owns this new prompt. Retire only the old
                // turn's stop fence; a duplicate admission must never undo a
                // pause/stop requested for the current turn.
                if (admitNewPrompt && event.promptId) {
                  const transcripts = useSessionStore.getState();
                  transcripts.resumeSession(event.sessionId);
                  transcripts.startStreaming(event.sessionId, event.promptId);
                  transcripts.clearReplaySuppression(event.sessionId);
                }
              }
              const owner = automationSessionRunsRef.current.get(event.sessionId);
              const transcript = useSessionStore.getState().transcripts[event.sessionId];
              const activePromptId = transcript?.pendingSendNowPromptId
                ?? transcript?.messages.slice().reverse().find(message => message.role === "assistant")?.promptId;
              const finishedPromptId = event.promptId ?? owner?.promptId;
              const staleFinish = event.phase === "finished" && (
                (owner && owner.recordId !== event.recordId)
                || (finishedPromptId && activePromptId && finishedPromptId !== activePromptId)
              );
              if (staleFinish) return;
              const status = event.status === "failed"
                ? "failed"
                : event.status === "success"
                  ? "completed"
                  : event.status === "stopped" || event.status === "interrupted"
                    ? "stopped"
                    : "working";
              sessionStore.upsert({
                sessionId: event.sessionId,
                cwd: event.cwd,
                ...(newAdmission && event.projectId ? { projectId: event.projectId } : {}),
                ...(event.currentModelId && (newAdmission || !existing?.currentModelId)
                  ? { currentModelId: event.currentModelId }
                  : {}),
                // Use the automation name to hydrate a brand-new background
                // row, but never replace a title already generated for it.
                ...(existing ? {} : { title: event.automationName }),
                ...(admitNewPrompt || !isControlledSessionStatus(existing?.status) ? { status } : {}),
                updatedAt: new Date().toISOString(),
              });
              if (event.projectId && event.phase === "sessionCreated") {
                const projects = useProjectsStore.getState().projects;
                const project = projects.find(p => p.id === event.projectId);
                if (project && !projects.some(item => projectOwnsSession(item, event.sessionId!))) {
                  useProjectsStore.getState().addConversation(project.id, { sessionId: event.sessionId, title: `定时：${event.automationName}`, createdAt: new Date().toISOString(), modelId: event.currentModelId });
                }
              }
              // Launch/transport failures may have no public prompt-complete
              // event. Close only the identified occurrence, keeping a visible
              // terminal bubble instead of leaving an empty live placeholder.
              if (event.phase === "finished" && finishedPromptId
                && (event.status === "failed" || event.status === "stopped" || event.status === "interrupted")) {
                const summarized = event.recoverySummaryStatus === "completed";
                const transcriptStore = useSessionStore.getState();
                const completion = {
                  sessionId: event.sessionId,
                  promptId: finishedPromptId,
                  stopReason: summarized ? "end_turn" : event.status === "failed" ? "error" : "cancelled",
                  agentResult: summarized ? undefined : event.error,
                };
                const settled = transcriptStore.finalizeTurn(completion);
                if (settled) {
                  // A transport failure can omit the public completion entirely.
                  // Retain queued input for an explicit retry without consuming
                  // the public rail's later metadata/usage acknowledgement.
                  const queue = useMessageQueueStore.getState();
                  queue.holdSending(event.sessionId, finishedPromptId, true, event.error);
                  queue.pauseQueued(event.sessionId);
                }
                if (settled && !summarized && event.error && transcriptStore.sessionId === event.sessionId) {
                  useSessionStore.getState().setError(event.error, { sessionId: event.sessionId, promptId: finishedPromptId });
                }
              }
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
                status: pendingSessionInteractionStatus(sessionId) ?? "working",
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
            if (!e.isReplay && (e.phase === "spawned" || e.phase === "finished")) {
              setTaskRefreshSignal(value => value + 1);
            }
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
            const msg = e.kind === "refusal" || e.kind === "content_filter"
              ? "模型服务因内容过滤结束了本轮回复。可调整请求后重试。"
              : e.detail
                ? friendlyError(e.detail)
              : e.kind === "rate_limit" || e.kind === "rate_limited"
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
            let activePromptId: string | null | undefined;
            try {
              const attachment = await agentRecoverSession(entry.sessionId, entry.cwd);
              loadedModel = attachment.modelId;
              activePromptId = attachment.activePromptId;
              historyLoaded = true;
              rendererHistoryPending.current = false;
            } finally {
              const status = findSessionSummary(entry.sessionId)?.status;
              const pendingInteraction = pendingSessionInteractionStatus(entry.sessionId);
              const adopted = activePromptId && sessionStore.getState().adoptRecoveredTurn(entry.sessionId, activePromptId);
              const knownIdle = activePromptId === null && !pendingInteraction;
              if (adopted && !pendingInteraction) {
                sessionsStore.getState().upsert({ sessionId: entry.sessionId, status: "working" });
              } else if (!agentSessionLoadInFlight(entry.sessionId)
                && (knownIdle || !isAgentOwnedActiveStatus(status))) {
                sessionStore.getState().finalizeIncompleteReplay(entry.sessionId, historyLoaded && status === "completed");
                const transcript = sessionStore.getState().transcripts[entry.sessionId];
                if (knownIdle && isAgentOwnedActiveStatus(status) && !transcript?.streamingMessageId && !transcript?.control) {
                  const latest = transcript?.messages.slice().reverse().find(message => message.role === "assistant");
                  const terminal = latest?.complete && latest.stopReason
                    ? terminalSessionStatus({
                      stopReason: latest.stopReason,
                      cancelTrigger: latest.cancelTrigger,
                      cancellationCategory: latest.cancellationCategory,
                    }) : "stopped";
                  sessionsStore.getState().upsert({ sessionId: entry.sessionId, status: terminal === "working" ? "stopped" : terminal });
                }
              }
              if (!agentSessionLoadInFlight(entry.sessionId)) sessionStore.getState().clearReplaySuppression(entry.sessionId);
            }
            if (disposed) return;
            if (sessionsStore.getState().currentSessionId === entry.sessionId && !sessionsStore.getState().deletedSessionIds.includes(entry.sessionId)) {
              sessionsStore.getState().upsert({ sessionId: entry.sessionId, currentModelId: loadedModel ?? entry.currentModelId });
              setSessionRestore(pending => pending?.sessionId === entry.sessionId && pending.error !== undefined ? null : pending);
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
  const currentProject = useProjectsStore(state => currentSessionId
    ? state.projects.find(project => projectOwnsSession(project, currentSessionId))
    : undefined);
  const currentTitle = currentProject
    ? projectSessionTitle(currentEntry?.title, currentProject.conversations.find(item => item.sessionId === currentSessionId)?.title)
    : currentEntry?.title || "";
  const activeSessionCwd = currentEntry?.cwd;
  const streaming = sessionStore((s) => s.streaming);
  const newSessionModelId = resolveConfiguredModelId(models, currentModelId);
  const activeSessionModelId = resolveSessionModelId(models, currentEntry?.currentModelId);
  const currentRestore = sessionRestore?.sessionId === currentSessionId ? sessionRestore : null;
  const sessionRestoreError = currentRestore?.error;
  const sessionLoading = currentRestore !== null && sessionRestoreError === undefined;
  const sessionNotReady = currentRestore !== null;
  const modelConfigured = currentSessionId
    ? activeSessionModelId !== undefined
    : newSessionModelId !== undefined;
  const chatReady = !!init?.auth.ready && !!activeSessionModelId && !modelSwitching && !sessionNotReady;
  const runtimeSetupHint = init?.auth.reason ?? "请先在「设置 → 模型」检查可用模型和连接";
  const chatSetupHint = sessionLoading
    ? "正在恢复会话…"
    : sessionRestoreError !== undefined
    ? "会话恢复失败，可继续编辑草稿"
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
      preparing: setExitStage,
      failed: (message) => showToast(message, 8000),
      beforePrepare: async (isCurrent) => {
        if (!isCurrent()) return;
        await confirmAutomationEditorExit(() => confirmAutomationExit({
        title: "退出前舍弃自动化草稿？",
        description: "自动化任务还有未保存的输入。继续编辑会保留草稿；确认退出后，这些输入将被舍弃。",
        cancelLabel: "继续编辑",
        confirmLabel: "舍弃修改并退出",
        danger: true,
        }));
        if (!isCurrent()) return;
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
    setCapabilityVisit(null);
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
  const navigate = (label: string, afterNavigate?: () => void) => {
    navigateNow(label);
    afterNavigate?.();
  };
  const handleNavigate = (label: string) => {
    if (label === "已归档") { openSettings("archived"); return; }
    navigate(label);
  };
  const openComposerCapability = useCallback<OpenComposerCapability>((destination, cwd) => {
    capabilityReturnFocus.current = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    setCapabilityVisit({ destination, cwd });
  }, []);
  const returnToComposer = () => {
    setCapabilityVisit(null);
    const trigger = capabilityReturnFocus.current;
    capabilityReturnFocus.current = null;
    requestAnimationFrame(() => trigger?.focus({ preventScroll: true }));
  };
  const navigateFromComposerCapability = (label: string) => {
    if (label === CAPABILITY_SECTION_LABEL || label === "技能" || label === "连接器") {
      setCapabilityVisit(visit => visit && visit.destination.route !== label
        ? { ...visit, destination: { route: label } }
        : visit);
    } else {
      handleNavigate(label);
    }
  };
  const consumeCreateExpertRequest = useCallback(() => {
    setCapabilityVisit(visit => visit?.destination.route === CAPABILITY_SECTION_LABEL && visit.destination.create
      ? { ...visit, destination: { route: CAPABILITY_SECTION_LABEL } }
      : visit);
  }, []);
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

  const handleOpenProject = (projectId: string | null, focusDraft = false, tab: ProjectTab = "overview") => {
    navigate("项目", () => useProjectsStore.getState().setActiveProjectId(projectId, focusDraft, tab));
  };

  const ensureQuotaAllowsSend = (): boolean => {
    const config = loadQuotaConfig();
    if (!isQuotaBlocking(loadUsage(), config)) return true;
    const quota = checkQuota(loadUsage(), config!);
    showToast(`用量已达到 ${quota.limit.toLocaleString()} 的上限，请到「用量统计」调整`);
    return false;
  };

  const captureTaskCreation = () => ({
    selection: ++selectionGenerationRef.current,
    knowledge: useKnowledgeStore.getState(),
    organization: useOrgSessionStore.getState().boundaryRevision,
  });
  const assertTaskCreationKnowledge = (creation: ReturnType<typeof captureTaskCreation>) => {
    if (creation.knowledge.defaultSources.includes("organization")
      && useOrgSessionStore.getState().boundaryRevision !== creation.organization) {
      throw new Error("组织账号或权限已改变，本次消息尚未发送，请核对知识来源后重试");
    }
  };
  const bindCreatedTaskKnowledge = (sessionId: string, creation: ReturnType<typeof captureTaskCreation>) => {
    assertTaskCreationKnowledge(creation);
    useKnowledgeStore.getState().bindSessionSources(sessionId, true, creation.knowledge);
  };

  const discardUnstartedTask = async (sessionId: string, cwd: string) => {
    try { await agentDeleteSession(sessionId, cwd); }
    catch (error) {
      console.warn("[EchoAgent] failed to clean up an unstarted task", error);
      showToast("本次消息尚未发送，空任务清理失败，可稍后从任务列表删除", 6000);
    }
  };

  const handleSendNew = async (text: string, attachments: string[] = []): Promise<boolean> => {
    // Composer also locks its button while this promise is pending, but this
    // app-level guard survives a view remount and protects every programmatic
    // caller from creating duplicate sessions for one submission.
    if ((!text.trim() && attachments.length === 0) || newSessionPendingRef.current) return false;
    let modelId = requireConfiguredModel();
    if (!modelId) return false;
    if (!ensureQuotaAllowsSend()) return false;
    newSessionPendingRef.current = true;
    const creation = captureTaskCreation();
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
      const sessionId = pendingExpert?.definitionPath && pendingExpert.definitionRaw
        ? await agentNewSession(cwd, modelId, draftPermissionMode, undefined, { name: pendingExpert.name, path: pendingExpert.definitionPath, raw: pendingExpert.definitionRaw, ...(newSessionModelOverrideRef.current ? { modelOverride: newSessionModelOverrideRef.current } : {}) })
        : await agentNewSession(cwd, modelId, draftPermissionMode);
      createdSessionId = sessionId;
      if (pendingExpert) modelId = (await agentSessionInfo(sessionId))?.model || modelId;
      taskWorkspaceRequestRef.current = crypto.randomUUID();
      createdSessionId = sessionId;
      if (requestedMode !== "default") {
        await setAgentMode(sessionId, requestedMode);
      }
      if (pendingExpert) {
        const saved = await agentSetSessionExpert(sessionId, pendingExpert.expertId, pendingExpert.name, "local", pendingExpert.avatarLocal, pendingExpert.definitionPath, pendingExpert.definitionRaw);
        if (!saved) throw new Error("专家身份未能保存，请重试；本次消息尚未发送");
      }
      bindCreatedTaskKnowledge(sessionId, creation);
      // Creation may outlive the home composer. Keep the admitted task scoped
      // to its own transcript without taking the user away from newer navigation.
      const foreground = selectionGenerationRef.current === creation.selection;
      if (foreground) {
        setCurrentModelId(modelId);
        sessionsStore.getState().setCurrent(sessionId);
        setPlaceholderView(null);
        usePermissionModeStore.getState().resetHomeMode();
        sessionStore.getState().setSession(sessionId);
      }
      sessionsStore.getState().upsert({
        sessionId,
        title: deriveTitle(text),
        cwd,
        status: "working",
        currentModelId: modelId,
        permissionMode: draftPermissionMode,
      });

      // Check for pending expert — inject persona invisibly.
      const pending = pendingExpert;
      let textForAgent = text;
      if (pending && pending.prompt && !pending.definitionRaw) {
        // Wrap persona in hidden markers. EchoAgent sees it as instructions;
        // MessageItem strips it from display on history replay.
        textForAgent = `${EXPERT_PERSONA_BEGIN}\n${pending.prompt}\n${EXPERT_PERSONA_END}\n\n${text}`;
        // Bind expert to session for persistence + UI badge.
        sessionsStore.getState().upsert({ sessionId, expertId: pending.expertId, expertName: pending.name, expertAvatar: pending.avatarLocal });
      }

      if (pending) sessionsStore.getState().upsert({ sessionId, expertId: pending.expertId, expertName: pending.name, expertAvatar: pending.avatarLocal });

      const accepted = beginAgentTurn({
        sessionId,
        promptText: textForAgent,
        displayText: text,
        attachments,
        allowBackgroundSession: !foreground,
      });
      if (!accepted) return false;
      if (pending && usePendingExpertStore.getState().expert === pending) usePendingExpertStore.getState().clear();
      if (foreground) setNewTaskMode("default");
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
    if (useContinuationStore.getState().pending[sessionId]) {
      showToast("正在创建接续对话，请稍候再发送");
      return false;
    }
    if (hasPendingQuestionForSession(sessionId)) {
      showToast("请先回答当前问题");
      return false;
    }
    // Guard against double-send / send-during-streaming. Composer also guards
    // via its `streaming` prop, but that value can be stale within the same
    // render tick; the store flag is the source of truth. A second pushUser +
    // startStreaming would orphan an empty placeholder that never completes.
    if (sessionStore.getState().streaming) return false;
    if (modelSwitching || sessionNotReady) {
      showToast(sessionNotReady ? chatSetupHint : "正在切换模型，请稍候");
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
    if (preparingProjectTurns.current.has(sessionId)) return false;
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
      if (claimed.documentEdit) promptTextOverride = documentEditPrompt(claimed.documentEdit, claimed.text);
    }
    preparingProjectTurns.current.add(sessionId);
    const retry = () => {
      if (queuePromptId) useMessageQueueStore.getState().settleSending(sessionId, "retry", queuePromptId);
    };
    try {
      const candidate = prepareExistingProjectTurn(sessionId, promptTextOverride ?? sendText);
      const prepared = candidate instanceof Promise ? await candidate : candidate;
      const current = sessionStore.getState();
      if (current.sessionId !== sessionId || current.streaming || hasPendingQuestionForSession(sessionId)
        || useContinuationStore.getState().pending[sessionId]) {
        retry();
        return false;
      }
      const accepted = beginAgentTurn({
        sessionId,
        promptText: prepared.promptText,
        displayText: sendText,
        attachments: sendAttachments,
        promptId: queuePromptId,
        onRejected: () => {
          prepared.setPending(true);
          retry();
        },
      });
      if (accepted) prepared.setPending(false);
      else retry();
      return accepted;
    } catch (error) {
      retry();
      if (sessionStore.getState().sessionId === sessionId) sessionStore.getState().setError(friendlyError(error));
      if (findSessionSummary(sessionId)) sessionsStore.getState().upsert({ sessionId, status: "failed" });
      return false;
    } finally {
      preparingProjectTurns.current.delete(sessionId);
    }
  };

  const handlePrepareRetry = async (sessionId: string): Promise<boolean> => {
    if (useContinuationStore.getState().pending[sessionId]) return false;
    if (sessionStore.getState().transcripts[sessionId]?.streamingMessageId) return false;
    if (modelSwitching || sessionNotReady) {
      showToast(sessionNotReady ? chatSetupHint : "正在切换模型，请稍候");
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
    if (useContinuationStore.getState().pending[sessionId]) return false;
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
   * Replace the active turn without making the user press Stop first. Runtime
   * owns this as one atomic sendNow operation, so the cancelled completion of
   * the old turn cannot race a separate follow-up send.
   */
  const handleSendNowCurrent = async (
    text: string,
    attachments: string[] = [],
    queueItemId?: string,
  ): Promise<boolean> => {
    if ((!text.trim() && attachments.length === 0) || !currentSessionId) return false;
    const sessionId = currentSessionId;
    if (hasPendingQuestionForSession(sessionId)) {
      showToast("请先回答当前问题");
      return false;
    }
    const transcript = sessionStore.getState();
    if (!transcript.streaming || transcript.sendNowPending) return false;
    if (modelSwitching || sessionNotReady) {
      showToast(sessionNotReady ? chatSetupHint : "正在切换模型，请稍候");
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
    if (!ensureQuotaAllowsSend() || preparingProjectTurns.current.has(sessionId)) return false;
    const previousTurn = transcript.transcripts[sessionId]?.streamingMessageId;

    const promptId = createAgentPromptId();
    let sendText = text;
    let promptText = text;
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
      promptText = claimed.documentEdit ? documentEditPrompt(claimed.documentEdit, claimed.text) : claimed.text;
    }
    const hold = (message: string) => {
      if (!queueItemId) return;
      const queue = useMessageQueueStore.getState();
      queue.holdSending(sessionId, promptId, false, message);
      queue.pauseQueued(sessionId, "后续消息已暂停，请按顺序恢复发送");
    };
    preparingProjectTurns.current.add(sessionId);
    let prepared: Awaited<ReturnType<typeof prepareExistingProjectTurn>>;
    try {
      const candidate = prepareExistingProjectTurn(sessionId, promptText);
      prepared = candidate instanceof Promise ? await candidate : candidate;
      const latest = sessionStore.getState();
      const target = latest.transcripts[sessionId];
      if (latest.sessionId !== sessionId || target?.streamingMessageId !== previousTurn
        || !target?.streamingMessageId || target.pendingSendNowPromptId || target.control
        || hasPendingQuestionForSession(sessionId)) {
        hold("原回复已结束或会话已切换，请确认后继续发送");
        return false;
      }
    } catch (error) {
      const detail = friendlyError(error);
      hold(detail);
      showToast(`暂时无法立即发送：${detail}`, 6000);
      return false;
    } finally {
      preparingProjectTurns.current.delete(sessionId);
    }
    transcript.requestSendNow(promptId, sessionId);
    prepared.setPending(false);
    sessionsStore.getState().upsert({ sessionId, status: "working" });

    // PromptRequest resolves only after the replacement turn ends. Detach it
    // so the composer can clear immediately, just like a normal admitted turn.
    void agentSendNow(
      sessionId,
      prepared.promptText,
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
      if (!promptWasAdmitted) prepared.setPending(true);
      latest.rejectSendNow(promptId, sessionId);
      latest.markComplete({
        sessionId,
        promptId,
        stopReason: "error",
        agentResult: detail,
      });
      if (queueItemId) {
        // Keep the same row for explicit recovery. A rejected replacement must
        // not silently retry when the still-running previous turn completes.
        useMessageQueueStore.getState().holdSending(sessionId, promptId, promptWasAdmitted, detail);
      } else if (!promptWasAdmitted) {
        // A direct composer send that never reached Runtime still needs a safe
        // retry path. Once admitted, duplicating it would repeat side effects.
        useMessageQueueStore.getState().enqueue(sessionId, sendText, sendAttachments);
        useMessageQueueStore.getState().pauseQueued(sessionId);
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
    // ChatView uses native tool status to reject ordinary idle chat controls.
    // Once requested, always apply the native execution fence: Runtime's
    // The agent mode can be default while a browser/computer session is live.
    const request = sessionStore.getState().requestControl(sessionId, action, activePromptId);
    sessionsStore.getState().upsert({
      sessionId,
      status: action === "pause" ? "pausing" : "stopping",
      updatedAt: new Date().toISOString(),
    });
    setCancellingSessionId(sessionId);
    try {
      await agentCancel(sessionId, action, activePromptId, beforeCancel?.pendingSendNowPromptId ? activeMessage?.promptId : undefined);
      // Runtime can acknowledge a named request without cancelling a newer
      // turn. Only the owner of the current fence may settle the session.
      const receipt = sessionControlReceiptState(sessionId, request);
      if (!receipt) return false;
      if (receipt === "pending") sessionStore.getState().confirmControl(sessionId, action);
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
      const receipt = sessionControlReceiptState(sessionId, request);
      if (!receipt) return false;
      if (receipt === "pending") {
        sessionStore.getState().rejectControl(sessionId, action);
        const transcript = sessionStore.getState().transcripts[sessionId];
        void restoreSessionInteractions(
          sessionId,
          transcript?.streamingMessageId || transcript?.pendingSendNowPromptId ? "working" : "failed",
        );
      }
      if (sessionStore.getState().sessionId === sessionId) {
        sessionStore.getState().setError(friendlyError(e));
      }
      showToast(`${action === "pause" ? "暂停" : "停止"}失败：${friendlyError(e)}`, 5000);
      return false;
    } finally {
      setCancellingSessionId((pending) => pending === sessionId ? null : pending);
    }
  };

  // Share the sidebar's persisted rename flow so every title surface updates
  // together. Rethrow failures so TopbarTitle restores the current name.
  const handleRenameTitle = async (newTitle: string) => {
    if (!currentEntry) return;
    try {
      await handleRenameSession(currentEntry.sessionId, newTitle, currentEntry.cwd);
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
    if (modelSwitching || sessionNotReady) return;

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

  const handleNewSession = () => {
    selectionGenerationRef.current += 1;
    setCapabilityVisit(null);
    setPlaceholderView(null);
    sessionsStore.getState().setCurrent(null);
    sessionStore.getState().reset();
    usePermissionModeStore.getState().resetHomeMode();
    setNewTaskMode("default");
    setCurrentModelId(resolveConfiguredModelId(models, newSessionModelOverrideRef.current, init?.auth.defaultModelId));
  };
  const handleStartOrganizationConversation = () => {
    const knowledge = useKnowledgeStore.getState();
    knowledge.setDefaultSources(["organization"]);
    knowledge.setDefaultOrganizationScopeIds([]);
    handleNewSession();
  };

  /** Navigate to home page without resetting session state (used after expert summon). */
  const handleGoHome = () => {
    setCapabilityVisit(null);
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
    manuallyNamedSessionsRef.current.add(sessionId);
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
    if (!findSessionSummary(sessionId)) {
      try {
        const entry = await agentFindSessionSummary(sessionId);
        if (entry) sessionsStore.getState().mergeSessions([entry]);
        else setHistoryIncomplete(true);
      } catch (error) {
        setHistoryIncomplete(true);
        showToast(`任务状态已更新，暂时无法读取会话摘要：${friendlyError(error)}`, 6000);
      }
    }
    if (next) showArchivedSessionActions(sessionId);
  };

  const handleDeleteSession = async (sessionId: string, explicitCwd?: string) => {
    const summary = findSessionSummary(sessionId);
    const cwd = summary?.cwd || explicitCwd;
    const result = await agentDeleteSession(sessionId, cwd);
    forgetDocumentSessions(sessionId);
    // Leave while the catalog still identifies this as the current session;
    // `remove` also clears currentSessionId and would otherwise strand its transcript.
    clearDeletedSessionState(sessionId);
    manuallyNamedSessionsRef.current.delete(sessionId);
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
    _sessionCwd?: string,
    preservePlaceholder = false,
    strict = false,
    intent?: SessionSelectionIntent,
  ) => {
    const focusedSessionId = sessionsStore.getState().currentSessionId;
    if (intent?.kind === "subagent" && focusedSessionId !== intent.source.parentSessionId) return;
    if (intent?.kind === "return" && focusedSessionId !== intent.point.childSessionId) return;
    const isSubagentNavigation = intent?.kind === "subagent" || intent?.kind === "return";
    const refreshingFocusedSession = preservePlaceholder && sessionId === focusedSessionId;
    const generation = ++selectionGenerationRef.current;
    if (!isSubagentNavigation && !preservePlaceholder) {
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
    let entry = findSessionSummary(sessionId);
    // Children and older tasks may be outside the loaded history pages.
    if (!entry) {
      try {
        const persisted = await agentFindSessionSummary(sessionId);
        if (selectionGenerationRef.current !== generation) {
          if (strict) throw new SessionNavigationSupersededError();
          return;
        }
        if (persisted) {
          sessionsStore.getState().mergeSessions([persisted]);
          entry = findSessionSummary(sessionId);
        }
      } catch (error) {
        if (selectionGenerationRef.current === generation) {
          showToast(`加载会话信息失败：${friendlyError(error)}`, 6000);
        }
        if (strict) throw error;
        return;
      }
    }
    if (selectionGenerationRef.current !== generation) {
      if (strict) throw new SessionNavigationSupersededError();
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
    const persistedModelId = entry.currentModelId;
    const selectedModelId = resolveSessionModelId(models, persistedModelId);
    if (!preservePlaceholder) setPlaceholderView(null);
    if (intent?.kind === "project-work-item") {
      setProjectWorkItemSource({ projectId: intent.projectId, sessionId });
    } else if (!isSubagentNavigation && !refreshingFocusedSession) {
      setProjectWorkItemSource(null);
    }
    if (intent?.kind === "subagent") {
      const point = { ...intent.source, childSessionId: sessionId };
      setSubagentTrail((trail) => pushSubagentReturnPoint(trail, point));
      setSubagentScrollRestore(null);
    } else if (intent?.kind === "return") {
      setSubagentTrail((trail) => popSubagentReturnPoint(trail, intent.point));
      setSubagentScrollRestore({ ...intent.point, sequence: ++subagentRestoreSequenceRef.current });
    } else if (!refreshingFocusedSession) {
      // An unrelated task selection is not a move up the subagent hierarchy.
      setSubagentTrail([]);
      setSubagentScrollRestore(null);
    }
    setSessionRestore({ sessionId, generation });
    sessionsStore.getState().setCurrent(sessionId);
    // Reflect the model actually persisted by this session. Do not fall back to
    // the first configured model: that would only change the picker, not the
    // backend session, and could route the next prompt to stale runtime settings.
    setCurrentModelId(selectedModelId);
    // The native replay owner clears derived history once. Repeated navigation
    // may join that load after it has already restored part of the child list.
    // setSession no longer wipes the transcript — it just moves focus. If we
    // already have a cached transcript for this session it arms replay
    // suppression so EchoAgent's history re-stream can't duplicate/merge it; if we
    // don't (first open / post-restart) the upcoming replay fills the empty
    // transcript. Either way the focused mirror is refreshed in one step.
    sessionStore.getState().setSession(sessionId);
    let historyLoaded = false;
    let restoreError: string | undefined;
    try {
      // Load with the session's own cwd. Opening history must not re-aim the
      // working directory selected for the next new task.
      // The catalog can lag behind work admitted by the native scheduler or
      // another client. Use the resident turn identity even for an entry
      // whose last persisted status is idle.
      const attachment = await agentRecoverSession(sessionId, entry.cwd);
      if (attachment.activePromptId
          && !sessionsStore.getState().deletedSessionIds.includes(sessionId)
          && sessionStore.getState().adoptRecoveredTurn(sessionId, attachment.activePromptId)
          && !isAgentOwnedActiveStatus(findSessionSummary(sessionId)?.status)) {
        sessionsStore.getState().upsert({ sessionId, status: "working" });
      }
      historyLoaded = true;
      if (selectionGenerationRef.current !== generation) {
        if (strict) throw new SessionNavigationSupersededError();
        return;
      }
      // The load response is authoritative; older runtimes may omit models.
      const actualModelId = attachment.modelId || findSessionSummary(sessionId)?.currentModelId;
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
    } catch (e) {
      if (!historyLoaded) restoreError = friendlyError(e);
      else if (selectionGenerationRef.current === generation && sessionsStore.getState().currentSessionId === sessionId) {
        sessionStore.getState().setError(friendlyError(e));
      }
      if (strict) throw e;
    } finally {
      // Historical replay can omit its terminal turn update. A replay-created
      // assistant must not keep the Composer in a running state after load.
      // Cached live turns are protected by finalizeIncompleteReplay itself.
      if (!agentSessionLoadInFlight(sessionId)) sessionStore.getState().finalizeIncompleteReplay(
        sessionId,
        historyLoaded && findSessionSummary(sessionId)?.status === "completed",
      );
      // Replay window is over: a *new* turn's updates for this session must be
      // ingested again. (No-op when there was no cached transcript to suppress.)
      if (!agentSessionLoadInFlight(sessionId)) sessionStore.getState().clearReplaySuppression(sessionId);
      setSessionRestore(pending => pending?.generation !== generation ? pending
        : restoreError === undefined ? null : { ...pending, error: restoreError });
    }
  };

  const handleOpenSubagentSession = (sessionId: string, sessionCwd: string | undefined, source: SubagentOpenContext) =>
    handleSelectSession(sessionId, sessionCwd, false, false, { kind: "subagent", source });

  const handleReturnToParentSession = () => {
    const point = activeSubagentReturnPoint(subagentTrail, sessionsStore.getState().currentSessionId);
    if (point) void handleSelectSession(point.parentSessionId, point.parentCwd, false, false, { kind: "return", point });
  };

  notificationNavigatorRef.current = (sessionId) => handleSelectSession(sessionId, undefined, false, true);
  // Catalog surfaces close only after history has actually loaded. Deliberate
  // cancellation keeps their selection available without an error banner.
  const openCatalogSession = (sessionId: string, cwd?: string) => handleSelectSession(sessionId, cwd, false, true);
  const openTrayGroup = async (groupId: string) => {
    const generation = ++selectionGenerationRef.current;
    const ensureCurrentNavigation = () => {
      if (selectionGenerationRef.current !== generation) throw new TrayActionCancelled();
    };
    ensureCurrentNavigation();
    if (useGroupStore.getState().groups.find(group => group.id === groupId)?.archived) throw new Error("专家团已归档");
    await useGroupStore.getState().select(groupId);
    ensureCurrentNavigation();
    const group = useGroupStore.getState();
    if (group.current?.id !== groupId || group.current.archived || group.error) throw new Error(group.error || "专家团已删除、归档或暂时无法打开");
    navigateNow("专家团");
  };
  useDesktopTray(Boolean(init?.ok), async (action) => {
    if (action.kind === "session") {
      if (!action.targetId) throw new Error("任务入口无效");
      const entry = findSessionSummary(action.targetId);
      if (entry?.archived || sessionsStore.getState().deletedSessionIds.includes(action.targetId)) throw new Error("任务已删除或归档，请查看全部任务");
      await openCatalogSession(action.targetId);
      setTrayAttention(previous => ({ sessionId: action.targetId!, sequence: (previous?.sequence ?? 0) + 1 }));
    } else if (action.kind === "group") {
      if (!action.targetId) throw new Error("专家团入口无效");
      await openTrayGroup(action.targetId);
    } else if (action.kind === "new") {
      handleNewSession();
      setTrayNewRequest(value => value + 1);
    } else if (action.kind === "all") {
      openSearch();
      setSidebarCollapsed(false);
    } else { throw new Error("不支持的托盘操作"); }
    setSettingsOpen(false);
    setAboutOpen(false);
    if (action.kind !== "all") setSearchOpen(false);
  }, (error, retry) => showActionToast(`任务菜单操作未完成：${friendlyError(error)}`, [{ label: "重试", onClick: retry }], 8000));
  useEffect(() => {
    if (!trayNewRequest || currentSessionId || placeholderView) return;
    const frame = requestAnimationFrame(() => document.querySelector<HTMLTextAreaElement>(".home textarea")?.focus());
    return () => cancelAnimationFrame(frame);
  }, [trayNewRequest, currentSessionId, placeholderView]);
  notificationAutomationNavigatorRef.current = async (automationId) => {
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
              if (open.automationId && !open.requestId) await notificationAutomationNavigatorRef.current(open.automationId);
              else if (open.sessionId) await notificationNavigatorRef.current(open.sessionId);
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
    try {
      await agentReloadSession(rewoundSessionId, entry.cwd);
      if (sessionsStore.getState().currentSessionId === rewoundSessionId) {
        setTaskRefreshSignal((value) => value + 1);
      }
    } catch (error) {
      if (sessionsStore.getState().currentSessionId === rewoundSessionId) {
        sessionStore.getState().setError(friendlyError(error));
      }
      throw error;
    } finally {
      if (!agentSessionLoadInFlight(rewoundSessionId)) sessionStore.getState().clearReplaySuppression(rewoundSessionId);
    }
  };

  // Fork copies the session to a new id — jump to it so the user sees the
  // branch they just created (and it appears in the sidebar).
  const continuationNavigationGeneration = selectionGenerationRef.current;
  const handleForked = async (newId: string, sourceSessionId: string, sourceCwd?: string, summary?:SessionSummary) => {
    const source = findSessionSummary(sourceSessionId);
    const cwd = source?.cwd ?? sourceCwd;
    if (!cwd) {
      showToast("接续对话已创建，但缺少工作目录信息，请刷新会话列表");
      void refreshSessionCatalog(true);
      return;
    }
    const modelId = resolveSessionModelId(models, source?.currentModelId);
    const fork: SessionSummary = {
      sessionId: newId,
      title: `${source?.title || "原对话"} · 新方向`,
      cwd,
      currentModelId: modelId,
      permissionMode: source?.permissionMode ?? "ask",
      ...summary,
    };
    // The Runtime fork already exists. A failed project save must retain it
    // as a recoverable task rather than report that creating the fork failed.
    sessionsStore.getState().upsert(fork);
    const knowledge = useKnowledgeStore.getState();
    if (knowledge.sessionSources[newId] === undefined) {
      knowledge.setSessionSources(newId, [...(knowledge.sessionSources[sourceSessionId] ?? [])]);
    }
    if (knowledge.sessionOrganizationScopeIds[newId] === undefined) {
      knowledge.setSessionOrganizationScopeIds(newId, [...(knowledge.sessionOrganizationScopeIds[sourceSessionId] ?? [])]);
    }
    try {
      const projects = useProjectsStore.getState();
      if (!projects.projects.some(project => project.conversations.some(row => row.sessionId === newId))) {
        await projects.associateForkWithSourceProject(sourceSessionId, fork);
      }
    } catch (error) {
      const linked = useProjectsStore.getState().projects.some((project) =>
        project.conversations.some((conversation) => conversation.sessionId === newId),
      );
      showToast(linked
        ? `接续对话已创建并保留，项目关联尚未确认保存。请在项目页重试保存：${friendlyError(error)}`
        : `接续对话已创建并保留为独立会话，关联项目失败。可在会话菜单“移入项目…”重试：${friendlyError(error)}`, 7000);
    }
    // A slow fork must not hijack a conversation the user selected meanwhile.
    if (sessionsStore.getState().currentSessionId !== sourceSessionId
      || selectionGenerationRef.current !== continuationNavigationGeneration) return;
    const hadHistory = Boolean(sessionStore.getState().transcripts[newId]?.messages.length);
    try {
      await handleSelectSession(newId, cwd, false, true);
      if (sessionsStore.getState().currentSessionId === newId) setConversationNavigation({sessionId:newId, sequence:++conversationNavigationSequence.current, focusComposer:true});
    } catch (error) {
      // A newer selection can join this same successful native replay. The
      // superseded caller must not discard its history or display an error.
      if (error instanceof SessionNavigationSupersededError) return;
      // A failed first replay is a partial cache, never authoritative history.
      // Reopening must read all persisted chunks instead of suppressing them.
      if (!hadHistory) {
        sessionStore.getState().dropSessionCache(newId);
        if (sessionStore.getState().sessionId === newId) sessionStore.getState().setError(friendlyError(error));
      }
      showActionToast(`新对话已创建并保留，暂时无法载入：${friendlyError(error)}`, [
        {label:"重新打开", onClick:() => { void handleSelectSession(newId, cwd); }},
      ], 7000);
    }
  };

  const handleOpenContinuationSession = async (id:string, target?:ContinuationTarget) => {
    const sourceId = sessionsStore.getState().currentSessionId;
    try {
      await handleSelectSession(id, undefined, false, true);
      if (sessionsStore.getState().currentSessionId === id) setConversationNavigation({sessionId:id, sequence:++conversationNavigationSequence.current, target});
    } catch (error) {
      if (error instanceof SessionNavigationSupersededError) return;
      if (sessionsStore.getState().currentSessionId === sourceId || sessionsStore.getState().currentSessionId === id) {
        if (findSessionSummary(id)?.archived) {
          showActionToast("关联对话已归档，请先恢复后查看", [{label:"管理归档",onClick:()=>openSettings("archived")}],6000);
        } else showToast(`无法打开关联对话：${friendlyError(error)}`, 6000);
      }
    }
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
        await memoryRecordSave({ scope: remember.scope, kind: remember.scope === "global" ? "user" : "project", key: memoryTitleFromContent(remember.content), content: remember.content, pinned: true, validUntil: null }, memoryCwd || undefined);
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
          showToast("无法接续：当前会话缺少工作目录信息");
          return false;
        }
        const live = sessionStore.getState();
        const permission = Boolean(usePermissionStore.getState().queues[sessionId]?.length || usePermissionStore.getState().queues.__global?.length);
        const question = Boolean(useQuestionStore.getState().queues[sessionId]?.length || useQuestionStore.getState().queues.__global?.length);
        if (live.streaming || live.sendNowPending || live.control?.phase === "pausing" || live.control?.phase === "stopping"
          || live.planApproval?.sessionId === sessionId || permission || question || sessionRestore?.sessionId === sessionId) {
          showToast("请等待当前执行和待处理操作结束后再继续"); return false;
        }
        const target = latestContinuationTarget(live.messages);
        const previous = slashContinuationRequests.current.get(sessionId);
        const request = previous && previous.targetPromptIndex === target?.promptIndex && previous.sourcePromptId === target?.promptId
          ? previous : {requestId:crypto.randomUUID(),targetPromptIndex:target?.promptIndex,sourcePromptId:target?.promptId};
        slashContinuationRequests.current.set(sessionId, request);
        const result = await sessionContinue(sessionId, cwd, request);
        await handleForked(result.session.sessionId, sessionId, cwd, result.session);
        slashContinuationRequests.current.delete(sessionId);
        if (result.warnings.length) showToast(result.warnings.join("；"), 7000);

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
        await handleRenameSession(sessionId, args.trim(), entry.cwd);

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

  const createProjectSession = async (project: ProjectMeta, modelId: string, options: ProjectConversationOptions) => {
    const requests = projectSessionRequestsRef.current;
    const previous = requests.get(project.id);
    if (previous?.pending) {
      showToast("正在创建项目对话，请稍候");
      return undefined;
    }
    const requestedCwd = options.cwd;
    const context = JSON.stringify([requestedCwd || null, modelId]);
    const request = previous?.context === context
      ? previous : { context, requestId: crypto.randomUUID(), pending: false };
    request.pending = true;
    requests.set(project.id, request);
    // A duplicate click does not take navigation ownership from its pending task.
    const creation = captureTaskCreation();
    try {
      const attachments = options.prepareAttachments ? await options.prepareAttachments() : [];
      assertTaskCreationKnowledge(creation);
      const cwd = requestedCwd || await agentPrepareTaskWorkspace(request.requestId);
      try {
        await agentsValidateReferences(cwd, project.experts);
      } catch (error) {
        if (isTaskWorkspaceError(error)) throw error;
        const recovery = project.cwd && cwd !== project.cwd
          ? "请换回项目默认目录，或核对项目默认专家能否在所选目录使用。"
          : "请检查项目默认专家配置和所选目录。";
        throw new Error(`无法验证项目专家。 ${recovery} ${String(error).replace(/^Error:\s*/, "")}`);
      }
      await flushProjectPersistence();
      const sessionId = await agentNewSession(cwd, modelId, options.requestedPermissionMode, options.expectedPermissionMode, undefined, project.id);
      requests.delete(project.id);
      return { sessionId, cwd, creation, attachments };
    } finally {
      request.pending = false;
    }
  };

  // 进入本地项目：在项目关联目录中新建会话，并注入项目说明作为种子消息。
  const handleStartProject = async (project: ProjectMeta, expectedPermissionMode: PermissionMode) => {
    const modelId = project.defaultModelId
      ? requireExplicitProjectModel(project.defaultModelId)
      : requireConfiguredModel();
    if (!modelId) return;
    if (!ensureQuotaAllowsSend()) return;
    let startedSessionId: string | undefined;
    let startedSessionCwd: string | undefined;
    try {
      const created = await createProjectSession(project, modelId, {
        cwd: project.cwd ?? null,
        expectedPermissionMode,
      });
      if (!created) return;
      const { sessionId, cwd, creation } = created;
      startedSessionId = sessionId;
      startedSessionCwd = cwd;
      const latestProject = useProjectsStore.getState().projects.find(item => item.id === project.id);
      if (!latestProject) throw new Error("原项目已删除，消息尚未发送");
      bindCreatedTaskKnowledge(sessionId, creation);
      if (!latestProject.defaultModelId) {
        useProjectsStore.getState().updateConfig(project.id, { defaultModelId: modelId });
      }
      if (selectionGenerationRef.current === creation.selection) {
        setPlaceholderView(null);
        setCurrentModelId(modelId);
        sessionsStore.getState().setCurrent(sessionId);
        sessionStore.getState().setSession(sessionId);
      }
      sessionsStore.getState().upsert({
        sessionId,
        title: project.name,
        cwd,
        projectId: project.id,
        status: "working",
        currentModelId: modelId,
        permissionMode: expectedPermissionMode,
      });
      // Register the session as a project conversation.
      useProjectsStore.getState().addConversation(project.id, {
        sessionId,
        title: project.name,
        createdAt: new Date().toISOString(),
        modelId,
      });
      const seed = `开始「${project.name}」项目，请先根据项目配置确认目标、约束和下一步。`;
      beginAgentTurn({ sessionId, promptText: seed, displayText: seed, allowBackgroundSession: true });
    } catch (e) {
      if (startedSessionId && !sessionsStore.getState().independent.some(entry => entry.sessionId === startedSessionId)) {
        if (startedSessionCwd) await discardUnstartedTask(startedSessionId, startedSessionCwd);
      } else if (startedSessionId) {
        applySessionScopedFailure({
          failedSessionId: startedSessionId,
          currentSessionId: sessionStore.getState().sessionId,
          message: friendlyError(e),
          setStatus: (sessionId, status) => sessionsStore.getState().upsert({ sessionId, status }),
          setCurrentError: (message) => sessionStore.getState().setError(message),
        });
      }
      if (friendlyError(e).startsWith("任务默认权限已变化")) {
        void usePermissionModeStore.getState().refreshProjectPermission();
      }
      if (isTaskWorkspaceError(e)) {
        throw new Error(`${String(e).replace(/^Error:\s*/, "")}。可以进入项目，选择其他工作目录后发送任务。`);
      }
      throw e;
    }
  };

  // Sending the project draft binds its workspace and creates the conversation.
  const handleStartProjectConversation: StartProjectConversation = async (projectId, message, requestedModelId, options) => {
    const project = useProjectsStore.getState().projects.find((p) => p.id === projectId);
    if (!project) throw new Error("原项目已删除，消息尚未发送");
    const modelId = requireExplicitProjectModel(requestedModelId);
    if (!modelId) return;
    if (!ensureQuotaAllowsSend()) return;
    let startedSessionId: string | undefined;
    let startedSessionCwd: string | undefined;
    try {
      const created = await createProjectSession(project, modelId, options);
      if (!created) return undefined;
      const { sessionId, cwd, creation, attachments } = created;
      startedSessionId = sessionId;
      startedSessionCwd = cwd;
      const latestProject = useProjectsStore.getState().projects.find(item => item.id === projectId);
      if (!latestProject) throw new Error("原项目已删除，消息尚未发送");
      bindCreatedTaskKnowledge(sessionId, creation);
      if (!latestProject.defaultModelId && options.rememberModel) {
        useProjectsStore.getState().updateConfig(projectId, { defaultModelId: modelId });
      }

      const title = deriveTitle(message);

      // Register conversation in the project.
      useProjectsStore.getState().addConversation(projectId, {
        sessionId,
        title,
        createdAt: new Date().toISOString(),
        modelId,
      });

      // Open only if this creation still owns the navigation intent.
      if (selectionGenerationRef.current === creation.selection) {
        setProjectWorkItemSource(options.sourceTab === "task" ? { projectId, sessionId } : null);
        setPlaceholderView(null);
        setCurrentModelId(modelId);
        sessionsStore.getState().setCurrent(sessionId);
        sessionStore.getState().setSession(sessionId);
      }
      sessionsStore.getState().upsert({
        sessionId,
        title,
        cwd,
        projectId,
        status: "working",
        currentModelId: modelId,
        permissionMode: options.expectedPermissionMode,
      });

      beginAgentTurn({ sessionId, promptText: message, displayText: message, attachments, allowBackgroundSession: true });
      return sessionId;
    } catch (e) {
      if (startedSessionId && !sessionsStore.getState().independent.some(entry => entry.sessionId === startedSessionId)) {
        if (startedSessionCwd) await discardUnstartedTask(startedSessionId, startedSessionCwd);
      } else if (startedSessionId) {
        applySessionScopedFailure({
          failedSessionId: startedSessionId,
          currentSessionId: sessionStore.getState().sessionId,
          message: friendlyError(e),
          setStatus: (sessionId, status) => sessionsStore.getState().upsert({ sessionId, status }),
          setCurrentError: (message) => sessionStore.getState().setError(message),
        });
      }
      if (friendlyError(e).startsWith("任务默认权限已变化")) {
        void usePermissionModeStore.getState().refreshProjectPermission();
      }
      throw e;
    }
  };

  const handleStartTeamWork: StartTeamWork = async (project, task, requestedModel, capability) => {
    if (!canEditTeamProject(project.role)) throw new Error(project.role === "viewer" ? "当前项目权限只允许查看，请使用项目问答" : projectRoleUnavailable);
    const identity = captureTeamProjectOperation();
    if (await isPrivateMemoryLookup(task.title) || await isPrivateMemoryLookup(task.description)) {
      throw new Error(privateMemoryInSharedProject);
    }
    identity.assertCurrent();
    const model = requireExplicitProjectModel(requestedModel);
    if (!model) throw new Error("执行模型暂不可用，任务尚未接手，请配置模型后重试");
    if (!ensureQuotaAllowsSend()) throw new Error("当前配额不足，任务尚未接手");
    const selection = selectionGenerationRef.current;
    const localProject = capability?.localProjectId ? useProjectsStore.getState().projects.find(p => p.id === capability.localProjectId) : undefined;
    if (capability?.localProjectId && !localProject) throw new Error("原项目已删除");
    const attachments = await capability?.options?.prepareAttachments?.() ?? [];
    identity.assertCurrent();
    const permissionMode = capability?.options?.expectedPermissionMode ?? "ask";
    const cwd = capability?.options?.cwd || await agentPrepareTaskWorkspace(crypto.randomUUID());
    if (localProject) await agentsValidateReferences(cwd, localProject.experts);
    identity.assertCurrent();
    let claimed = false;
    let claimAttempted = false;
    let sessionId: string | undefined;
    let groupId: string | undefined;
    const claim = async () => {
      identity.assertCurrent();
      claimAttempted = true;
      const updated = await teamProjectRequest<TeamProjectSnapshot>("claimTask", project.id, { expectedRevision: task.revision }, task.id, identity);
      identity.assertCurrent();
      claimed = true;
      const current = updated.tasks.find(item => item.id === task.id);
      if (!current) throw new Error("任务已不存在，请刷新项目");
      return `项目：${updated.name}\n任务：${current.title}\n${current.description}\n验收要求：${current.acceptance || "请与项目负责人确认"}\n请先核对项目最新依据，再执行此工作。执行完成后请说明成果文件和待验收事项。本轮公开输出会作为执行报告共享给项目成员；本机文件需要在项目资料中明确发布。`;
    };
    try {
      if (capability?.team) {
        const template = capability.team;
        const group = await groupCreate({ requestId: crypto.randomUUID(), name: `${project.name} · ${task.title}`.slice(0, 80), goal: task.description.slice(0, 8000), cwd, modelId: capability.modelOverride ?? template.defaultModelId ?? model,
          expectedOrganizationGeneration: identity.expectedGeneration, projectId: project.id, projectTaskId: task.id,
          members: template.members.map(m => m.name), roles: Object.fromEntries(template.members.map(m => [m.name, m.role])),
          models: Object.fromEntries(template.members.filter(m => m.modelId).map(m => [m.name, m.modelId!])), coordinatorName: template.coordinatorName ?? undefined, coordinatorRole: template.coordinatorRole ?? undefined });
        groupId = group.id;
        const prompt = await claim();
        identity.assertCurrent();
        await groupSend(group.id, { id: crypto.randomUUID(), text: prompt, memberIds: [], discussion: false, attachments, replyTo: null });
        identity.assertCurrent();
        if (selectionGenerationRef.current === selection) {
          await useGroupStore.getState().select(group.id); identity.assertCurrent(); handleNavigate("专家团");
        }
        return group.id;
      }
      const expert = capability?.expert;
      sessionId = await agentNewSession(cwd, model, capability?.options ? capability.options.requestedPermissionMode : "ask", capability?.options?.expectedPermissionMode, expert?.path && expert.raw ? { name: expert.name, path: expert.path, raw: expert.raw, ...(capability?.modelOverride ? { modelOverride: capability.modelOverride } : {}) } : undefined);
      identity.assertCurrent();
      await bindTeamProjectSession(sessionId, project.id, task.id, identity);
      if (expert) await agentSetSessionExpert(sessionId, expert.name, expert.name, "local", undefined, expert.path, expert.raw);
      const actualModel = (await agentSessionInfo(sessionId))?.model || model;
      const sharedPrompt = await claim();
      const prompt = localProject ? buildProjectPrompt(localProject, sharedPrompt) : sharedPrompt;
      identity.assertCurrent();
      useKnowledgeStore.getState().setSessionSources(sessionId, ["organization"]);
      useKnowledgeStore.getState().setSessionOrganizationScopeIds(sessionId, [project.scopeId]);
      if (localProject) useProjectsStore.getState().addConversation(localProject.id, { sessionId, title: task.title, createdAt: new Date().toISOString(), modelId: actualModel });
      sessionsStore.getState().upsert({ sessionId, title: `${project.name} · ${task.title}`, cwd, currentModelId: actualModel, permissionMode, status: "working" });
      if (selectionGenerationRef.current === selection) {
        if (localProject) setProjectWorkItemSource({ projectId: localProject.id, sessionId });
        sessionsStore.getState().setCurrent(sessionId); sessionStore.getState().setSession(sessionId); setCurrentModelId(actualModel); setPlaceholderView(null);
      }
      if (!beginAgentTurn({ sessionId, promptText: prompt, displayText: task.title, attachments, allowBackgroundSession: true })) throw new Error("执行未能发送，请重新打开执行后重试");
      return sessionId;
    } catch (error) {
      if (!claimed) {
        if (sessionId) await discardUnstartedTask(sessionId, cwd);
        if (groupId) { try { await groupDelete(groupId, false); } catch { /* Keep the local group available for manual cleanup. */ } }
      } else if (sessionId) {
        sessionsStore.getState().upsert({ sessionId, title: `${project.name} · ${task.title}`, cwd, currentModelId: model, permissionMode, status: "failed" });
      }
      if (claimed && (sessionId || groupId)) showActionToast("任务已接手，请检查已有执行后继续", [{ label: "打开已有执行", onClick: () => {
        void (async () => {
          identity.assertCurrent();
          if (groupId) { await useGroupStore.getState().select(groupId); identity.assertCurrent(); handleNavigate("专家团"); }
          else if (sessionId) await handleSelectSession(sessionId, cwd, false, true);
        })().catch(error => showToast(friendlyError(error)));
      } }]);
      throw new Error(`${claimed ? "任务已接手，执行启动未确认；可打开已有执行检查后继续。" : claimAttempted ? "接手结果未确认，请刷新项目核对负责人后重试。" : "任务尚未接手。"}${friendlyError(error)}`);
    }
  };

  const handleOpenTeamExecution: OpenTeamExecution = async (project, run) => {
    const identity = captureTeamProjectOperation();
    const selection = selectionGenerationRef.current;
    const assertCurrent = () => {
      identity.assertCurrent();
      if (selectionGenerationRef.current !== selection) throw new SessionNavigationSupersededError();
    };
    const latest = await teamProjectRequest<TeamProjectSnapshot>("snapshot", project.id, undefined, undefined, identity);
    assertCurrent();
    if (!canEditTeamProject(latest.role)) throw new Error(latest.role === "viewer" ? "当前权限只允许查看共享报告" : projectRoleUnavailable);
    const current = latest.runs?.find(item => item.id === run.id);
    if (!current || current.actorId !== useOrgSessionStore.getState().session?.user?.id) throw new Error("请查看该执行的共享报告；继续工作需要接手后新建执行");
    const groups = await groupList();
    assertCurrent();
    if (groups.some(group => group.id === current.sessionId)) {
      await useGroupStore.getState().select(current.sessionId); assertCurrent(); handleNavigate("专家团"); return;
    }
    const local = await agentFindSessionSummary(current.sessionId);
    assertCurrent();
    if (!local) throw new Error("该执行不在本机或已删除。共享报告仍可查看；需要继续时，请从任务发起新执行。");
    const localProject = useProjectsStore.getState().projects.find(p => projectOwnsSession(p, current.sessionId));
    await handleSelectSession(current.sessionId, local.cwd, false, true, localProject ? { kind: "project-work-item", projectId: localProject.id } : undefined);
  };

  useEffect(() => {
    setCapabilityVisit(null);
    capabilityReturnFocus.current = null;
  }, [placeholderView, currentSessionId]);
  const activeNav = capabilityVisit?.destination.route ?? placeholderView ?? (currentSessionId ? "" : "新建任务");
  const activeSubagentParent = activeSubagentReturnPoint(subagentTrail, currentSessionId);
  const parentSessionTitle = activeSubagentParent
    ? findSessionSummary(activeSubagentParent.parentSessionId)?.title ?? "上级任务"
    : null;

  const placeholderProps = {
    defaultWorkspaceCwd: init?.cwd,
    onOpenAutomationSettings: (section: "general" | "notifications") => section === "notifications"
      ? openSettings("notify-channels") : openSettings("general", "background"),
    onOpenRetrievalSettings: () => openSettings("memory", "local-retrieval"),
    onNavigate: handleNavigate,
    onOpenSession: handleSelectSession,
    onGoHome: handleGoHome,
    onStartOrganizationConversation: handleStartOrganizationConversation,
    onToast: showToast,
    workspaces,
    sessionId: currentSessionId ?? undefined,
    notificationAutomationId: notificationAutomationOpen?.id,
    notificationAutomationSequence: notificationAutomationOpen?.sequence,
    projectModels: models,
    projectDefaultModelId: newSessionModelId,
    onProjectNavigate: handleProjectNavigation,
    onOpenProjectWorkItemSession: (projectId: string, sessionId: string, cwd?: string) =>
      handleSelectSession(sessionId, cwd, false, false, { kind: "project-work-item", projectId }),
    onOpenMeetingMinutes: () => handleNavigate("录音转写"),
    onOpenModelSettings: () => openSettings("model"),
    onClientSlashCommand: handleClientSlashCommand,
    onStartProject: handleStartProject,
    onStartProjectConversation: handleStartProjectConversation,
    onStartTeamWork: handleStartTeamWork,
    onOpenTeamExecution: handleOpenTeamExecution,
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
    <ModelManagementContext.Provider value={openModelManagement}>
    <ModelCatalogContext.Provider value={models}>
    <div className={`app${IS_MACOS ? " app--macos" : ""}`}>
      {exitStage && !automationExitDialog && <div className="app__exit-overlay" role="status" aria-live="polite">
        {exitStage === "recording" ? "正在保存录音…" : exitStage === "tasks" ? "正在保存任务数据…" : "正在退出…"}
      </div>}
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
          onOpenProject={handleOpenProject}
          onOpenProjectDraft={projectId => handleOpenProject(projectId, true)}
          onRenameSession={handleRenameSession}
          onArchiveSession={handleArchiveSession}
          onDeleteSession={handleDeleteSession}
          onRetrySessions={() => void refreshSessionCatalog(true)}
          sessionHistory={historyControls(false)}
          activeNav={activeNav}
        />
        <SidebarResizeHandle layout={sidebarLayout} />
        <main className="app__main">
          <PageChromeProvider macOS={IS_MACOS} sidebarCollapsed={sidebarCollapsed} onToggleSidebar={() => setSidebarCollapsed(!sidebarCollapsed)} onNewTask={handleNewSession}>
          <ComposerCapabilityNavigation.Provider value={openComposerCapability}>
          {/* Keep the source composer and its draft controls mounted during management. */}
          <div className="app__composer-source" hidden={Boolean(capabilityVisit)}>
          {(!currentSessionId || placeholderView) && !(init?.ok && !initError && ["项目", "组织", "专家·技能·连接器", "专家团", "技能", "连接器", "插件", "自动化", "知识库", "录音转写"].includes(placeholderView ?? "")) && (
            <header className="main-page-controls" data-tauri-drag-region>
              <SessionNavigation sidebarCollapsed={sidebarCollapsed} onToggleSidebar={() => setSidebarCollapsed(!sidebarCollapsed)} onNewTask={handleNewSession} />
            </header>
          )}
          {(!currentSessionId || placeholderView) && modelCatalogNotice}
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
          ) : (
            <Suspense fallback={<div className="app__notice" role="status">正在加载界面…</div>}>
              {placeholderView === "专家团" ? (
                <CapabilitySection active="专家团" onNavigate={handleNavigate} hideNavigation={groupChatOpen}>
                  <GroupWorkspace onOpenExperts={() => handleNavigate("专家·技能·连接器")} cwd={newSessionTargetCwd} modelId={newSessionModelId} models={models} workspaces={workspaces} onToast={showToast} onOpenSettings={() => openSettings("model")} onOpenKnowledgeBase={() => handleNavigate("知识库")} onOpenOrganization={() => handleNavigate("组织")} />
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
                  projectBreadcrumb={currentProject ? {
                    name: currentProject.name,
                    onOpenProjects: () => handleOpenProject(null),
                    onOpenProject: () => handleOpenProject(currentProject.id, false,
                      projectWorkItemSource?.projectId === currentProject.id && projectWorkItemSource.sessionId === currentSessionId
                        ? "task" : "overview"),
                  } : undefined}
                  attentionRequest={trayAttention?.sessionId === currentSessionId ? trayAttention.sequence : undefined}
                  notice={<>
                    {modelCatalogNotice}
                    {(legacySessions.has(currentSessionId) || /^代码开发[：:]/.test(currentEntry?.title ?? "")) && (
                      <div className="conversation-continuation-feedback" role="note">
                        <span>此会话来自已下线的代码工作台。你可以查看会话记录、原工作目录和已归档的草稿。</span>
                        <button type="button" onClick={() => openSettings("data", "legacy-workspace")}>查看迁移资料</button>
                      </div>
                    )}
                  </>}
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
                  setupRequiresSettings={!modelSwitching && (!init.auth.ready || models.length === 0)}
                  draftOnly={sessionNotReady}
                  sessionRestoreError={sessionRestoreError}
                  onRetrySessionRestore={() => { void handleSelectSession(currentSessionId, activeSessionCwd, true); }}
                  onOpenSecuritySettings={() => openSettings("security")}
                  modelId={activeSessionModelId}
                  modelLoading={sessionLoading || modelSwitching}
                  models={models}
                  onModelChange={handleModelChange}
                  cwd={activeSessionCwd}
                  defaultWorkspaceCwd={init.cwd}
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
                  onOpenContinuationSession={(id, target) => { void handleOpenContinuationSession(id, target); }}
                  conversationNavigation={conversationNavigation}
                  onToast={showToast}
                  onSelectExpert={handleStartWithExpert}
                  onOpenSubagentSession={handleOpenSubagentSession}
                  subagentScrollRestore={subagentScrollRestore}
                  onOpenKnowledgeBase={() => handleNavigate("知识库")}
                  onOpenMeetingMinutes={() => handleNavigate("录音转写")}
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
                  onToast={showToast}
                  onOpenKnowledgeBase={() => handleNavigate("知识库")}
                  onOpenMeetingMinutes={() => handleNavigate("录音转写")}
                  onOpenOrganization={() => handleNavigate("组织")}
                  commandRefreshKey={commandRefreshKey}
                  onClientSlashCommand={handleClientSlashCommand}
                  taskMode={newTaskMode}
                  onTaskModeChange={setNewTaskMode}
                />
              )}
            </Suspense>
          )}
          </div>
          </ComposerCapabilityNavigation.Provider>
          {capabilityVisit && (
            <Suspense fallback={<div className="app__notice" role="status">正在加载界面…</div>}>
              <PlaceholderPage
                key={capabilityVisit.destination.route}
                {...placeholderProps}
                label={capabilityVisit.destination.route}
                cwd={capabilityVisit.cwd}
                onNavigate={navigateFromComposerCapability}
                onCapabilityBack={returnToComposer}
                createExpertRequested={capabilityVisit.destination.route === CAPABILITY_SECTION_LABEL && capabilityVisit.destination.create}
                onCreateExpertRequestHandled={consumeCreateExpertRequest}
                configureConnector={capabilityVisit.destination.route === "连接器" && capabilityVisit.destination.configure}
                initialConnectorName={capabilityVisit.destination.route === "连接器" ? capabilityVisit.destination.serverName : undefined}
              />
            </Suspense>
          )}
          </PageChromeProvider>
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
          onOpen={() => handleNavigate("录音转写")}
          onToast={showToast}
        />
      </Suspense>
      {searchOpen && (
        <Suspense fallback={null}>
          <SearchOverlay
            open
            onClose={() => setSearchOpen(false)}
            onSelect={openCatalogSession}
            onSelectGroup={openTrayGroup}
          />
        </Suspense>
      )}
      {settingsOpen && (
        <Suspense fallback={null}>
          <SettingsPanel
            open
            initialSection={settingsSection}
            initialModelId={settingsModelId}
            initialTarget={settingsTarget}
            sessionId={currentSessionId ?? undefined}
            cwd={activeSessionCwd || newSessionTargetCwd}
            onClose={() => setSettingsOpen(false)}
            onModelsChanged={refreshModels}
            onOpenOrganization={() => { setSettingsOpen(false); handleNavigate("组织"); }}
            onRestoreSession={handleArchiveSession}
            sessionHistory={historyControls(true)}
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
    </ModelCatalogContext.Provider>
    </ModelManagementContext.Provider>
  );
}
