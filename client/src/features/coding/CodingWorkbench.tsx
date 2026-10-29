import { ManagedWorktrees } from "./panels/ManagedWorktrees";
import { ProjectChecksEditor } from "./panels/ProjectChecksEditor";
import { RunHistory } from "./panels/RunHistory";
import type { CodingTurnOptions } from "@/lib/agent-client";
import type { EditorContextSnapshot } from "./lib/workspace-editor";
import { EditablePlan } from "./agent/EditablePlan";
import type { CodingIntent } from "./lib/workflow";
import { ExternalToolRecovery } from "./panels/ExternalToolRecovery";
import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type CSSProperties,
} from "react";
import {
  FolderGit2,
  GitBranch,
  LoaderCircle,
  PanelRightClose,
  PanelRightOpen,
  PanelTop,
  Settings2,
  Check,
  Square,
  X,
} from "lucide-react";

import { useAppDialog } from "@/components/AppDialog";
import { ActionMenu } from "@/components/ActionMenu";
import type { ModelOption } from "@/components/ModelSelector";
import { usePermissionStore } from "@/stores/permission-store";
import { useQuestionStore } from "@/stores/question-store";
import { useSessionStore } from "@/stores/session-store";
import { filesystemPickDirectory } from "@/lib/agent-client";

import { QueuePanel } from "@/components/QueuePanel";
import { PlanPanel } from "@/components/PlanPanel";
import { useCodingFollowupQueue } from "./hooks/useCodingFollowupQueue";
import {
  claimCodingAdmission,
  codingQueueId,
  codingQueueOwner,
  MANUAL_FOLLOWUP_PHASES,
  sendCodingQueuedFollowup,
} from "./lib/coding-followup";
import { useMessageQueueStore } from "@/stores/message-queue-store";
import { AgentPane } from "./agent/AgentPane";
import { TheiaAgentComposer } from "./agent/TheiaAgentComposer";
import { TheiaIdeFrame, type TheiaMutationTicket } from "./TheiaIdeFrame";
import { ChangeSetView } from "./explorer/ChangeSetView";
import {
  codingTaskContextDraftKey,
  codingTaskDraftKey,
} from "./lib/task-draft-key";
import { useTheiaWorkbenchBridge } from "./hooks/useTheiaWorkbenchBridge";
import { idePath, relativeToWorkspace } from "./lib/windows-path";
import { useCodingMutationLifecycle } from "./hooks/useCodingMutationLifecycle";
import {
  buildCodingWorkflowPrompt,
  buildNodeContinuationInstruction,
  mergeTaskVerificationCommands,
} from "./lib/workflow";
import { describeTaskProgress, isBusyPhase } from "./lib/phase";
import {
  codingApi,
  onPhaseChanged,
  onVerificationOutput,
  onVerificationUpdated,
} from "./lib/tauri-api";
import {
  type DeliveryReport,
  type DetectedCommand,
  type IsolatedWorkspace,
  type TddEvidence,
  type VerificationRecord,
} from "./lib/types";
import {
  codingRuntimeKey,
  assertCodingWorkspaceCancellationClear,
  codingWorkspaceCancellationBlocked,
  codingWorkspaceCancellationPending,
  DOCUMENT_CANCELLATION_PENDING_MESSAGE,
  DOCUMENT_CANCELLATION_CHECKING_MESSAGE,
  useCodingRuntimeStore,
} from "./lib/coding-task-runtime";
import { useCodingActionScope } from "./hooks/useCodingActionScope";
import {
  EditorPreparationCancelled,
  prepareWorkspaceEditor,
  withWorkspaceDiskMutation,
  workspaceEditor,
} from "./lib/workspace-editor";
import { useVerificationRunner } from "./lib/use-verification-runner";
import { DeliveryReportTab } from "./main/docs/DeliveryReportTab";
import { FindReferencesView } from "./main/FindReferencesView";
import { ImpactAnalysisView } from "./main/ImpactAnalysisView";
import { TaskDagTab } from "./main/docs/TaskDagTab";
import { VerificationView } from "./panels/VerificationView";
import { ProjectSwitcher } from "./shell/ProjectSwitcher";
import { WorkbenchIdentity } from "./shell/WorkbenchIdentity";
import { TaskSwitcher } from "./shell/TaskSwitcher";
import { useTaskStore } from "./store/task-store";

interface CodingWorkbenchProps {
  cwd?: string;
  workspaces?: { cwd: string }[];
  onSelectWorkspace?: (cwd: string) => void;
  /** Recently opened coding projects shown by the current-project switcher. */
  codingWorkspaces?: { cwd: string }[];
  /** Current coding project. One workbench window has one active project. */
  activeCodingWorkspaceCwd?: string;
  /** Remove a path from recent projects without deleting its files. */
  onCloseCodingWorkspace?: (cwd: string) => void;
  onToast?: (message: string) => void;
  onExit?: () => void;
  onRegisterLeaveGuard?: (guard: (() => Promise<boolean>) | null) => void;
  onOpenSettings?: () => void;
  models?: ModelOption[];
  defaultModelId?: string;
  /** True once a model and credentials are configured. */
  apiReady?: boolean;
  /**
   * Session lifecycle stays with the host (App.tsx) so the whole application
   * creates and cancels Agent sessions in one place. Live transcript state is
   * read from the shared stores rather than threaded through props.
   */
  sessionId?: string | null;
  onStartRun?: (
    root: string,
    requirement: string,
    modelId: string | undefined,
    contextPaths: string[],
    onSessionReady: (sessionId: string) => Promise<void>,
    promptTextOverride?: string,
    coding?: CodingTurnOptions,
  ) => Promise<string | undefined>;
  /** Focus a task's persisted Agent session without leaving the workbench. */
  onActivateSession?: (sessionId: string, cwd: string) => Promise<void>;
  onChangeModel?: (modelId: string) => void | Promise<void>;
  onSendMessage?: (
    text: string,
    promptTextOverride?: string,
    coding?: CodingTurnOptions,
  ) => boolean | void | Promise<boolean | void>;
  onCorrectRun?: (text: string) => boolean | Promise<boolean>;
  onCancelRun?: () => boolean | void | Promise<boolean | void>;
}

function basename(path: string): string {
  const segments = path.replace(/\\/g, "/").split("/").filter(Boolean);
  return segments[segments.length - 1] ?? path;
}

function normalizedRelativePath(path: string): string {
  return path.replace(/\\/g, "/").replace(/^\.\//, "");
}

function workspaceRelativePath(root: string, path: string): string {
  return normalizedRelativePath(
    relativeToWorkspace(root, path) ?? idePath(path),
  );
}
function absoluteWorkspacePath(root: string, path: string): string {
  const base = idePath(workspaceEditor(root)?.workspace ?? root);
  const nativeRoot = base === "/" || /^[A-Za-z]:\/$/.test(base) ? base : base.replace(/\/+$/, "");
  const value = idePath(path).replace(/^\.\//, "");
  const relative = relativeToWorkspace(root, value) ?? relativeToWorkspace(nativeRoot, value);
  const prefix = nativeRoot.replace(/\/+$/, "");
  const absolute = relative !== null ? `${prefix}/${relative}`
    : value.startsWith("/") || /^[A-Za-z]:\//.test(value) ? value : `${prefix}/${value}`;
  if (!value || relativeToWorkspace(nativeRoot, absolute) === null) throw new Error("文件不属于当前工程");
  return absolute;
}
/**
 * Integrated coding workbench: repository navigation, editing, task-bound Agent
 * execution, verification, review and delivery in one persistent workspace.
 */
export function CodingWorkbench({
  cwd = "",
  workspaces = [],
  onSelectWorkspace,
  onToast,
  onExit,
  onRegisterLeaveGuard,
  onOpenSettings,
  models = [],
  codingWorkspaces,
  activeCodingWorkspaceCwd,
  onCloseCodingWorkspace,
  defaultModelId,
  apiReady = false,
  sessionId: hostSessionId = null,
  onStartRun,
  onActivateSession,
  onChangeModel,
  onSendMessage,
  onCancelRun,
  onCorrectRun,
}: CodingWorkbenchProps) {
  const {
    requestConfirmation: requestTaskConfirmation,
    confirm: confirmTaskAction,
    requestInput: requestTaskInput,
    dialog: taskDialog,
  } = useAppDialog(cwd);
  const isWorkspaceCurrent = useCodingActionScope(cwd);
  const startGeneration = useRef(0);
  useEffect(() => {
    startGeneration.current += 1;
    setStarting(false);
    setSending(false);
    setStartError(null);
  }, [cwd]);
  const prepareEditor = useCallback(async () => {
    assertCodingWorkspaceCancellationClear(cwd);
    const expectedTask = useTaskStore.getState().task?.id;
    await prepareWorkspaceEditor(cwd, async (count) => {
      const confirmed = await confirmTaskAction({
        title: `保存 ${count} 个文件后继续？`,
        description:
          "任务、验证和回滚将使用已保存的磁盘内容。取消会保留编辑内容；保存失败时不会继续。",
        confirmLabel: "保存并继续",
      });
      assertCodingWorkspaceCancellationClear(cwd);
      if (
        !confirmed ||
        !isWorkspaceCurrent() ||
        useTaskStore.getState().task?.id !== expectedTask
      )
        return false;
      const selected = useTaskStore.getState();
      // A queued verification phase forbids manual file writes. Pause the
      // scheduler before saving, while retaining the foreground batch claim.
      if (selected.root === cwd && selected.task?.phase === "verifying") {
        const key = codingRuntimeKey(cwd, selected.task.id);
        if (useCodingRuntimeStore.getState().activeRunIds[key])
          throw new Error("验证命令仍在运行，请停止后保存");
        await codingApi.reportInterrupted(cwd, selected.task.id, "paused");
        await selected.refreshTaskState();
      }
      return true;
    });
    assertCodingWorkspaceCancellationClear(cwd);
    const taskIsCurrent = () =>
      isWorkspaceCurrent() &&
      (expectedTask
        ? useTaskStore.getState().root === cwd &&
          useTaskStore.getState().task?.id === expectedTask
        : !useTaskStore.getState().task);
    if (!taskIsCurrent()) throw new Error("任务已切换，请在当前任务重试");
    assertCodingWorkspaceCancellationClear(cwd);
    // The save bridge itself advances the task revision. Read after save and
    // the final dirty ACK, then recheck ownership before changing task state.
    const saved = expectedTask
      ? await codingApi.getTask(cwd, expectedTask)
      : undefined;
    assertCodingWorkspaceCancellationClear(cwd);
    if (!taskIsCurrent()) throw new Error("任务已切换，请在当前任务重试");
    if (expectedTask)
      useCodingRuntimeStore.getState().setEditorBlock(cwd, expectedTask, null);
    return saved?.updatedAt;
  }, [cwd, confirmTaskAction, isWorkspaceCurrent]);
  const taskSessionId = useTaskStore((state) => state.task?.sessionId);
  // Never fall back to an unrelated current chat when a coding task has no
  // bound session. An explicit recovery state prevents cross-task sends.
  const activeSessionId = taskSessionId ?? null;
  // Live transcript for the task's session, read straight from the shared store.
  const transcript = useSessionStore((state) =>
    activeSessionId ? state.transcripts[activeSessionId] : undefined,
  );
  const messages = transcript?.messages ?? [];
  const streaming = useSessionStore((state) =>
    activeSessionId
      ? Boolean(state.transcripts[activeSessionId]?.streamingMessageId)
      : false,
  );
  const awaitingPermission = usePermissionStore((state) =>
    activeSessionId ? (state.queues[activeSessionId]?.length ?? 0) > 0 : false,
  );
  const awaitingQuestion = useQuestionStore((state) =>
    activeSessionId ? (state.queues[activeSessionId]?.length ?? 0) > 0 : false,
  );
  const recentCodingProjects = useMemo(() => {
    const paths = [
      activeCodingWorkspaceCwd || cwd,
      ...(codingWorkspaces ?? []).map((workspace) => workspace.cwd),
    ].filter(Boolean);
    return [...new Set(paths)].map((projectCwd) => ({ cwd: projectCwd }));
  }, [activeCodingWorkspaceCwd, codingWorkspaces, cwd]);
  const [theiaPanel, setTheiaPanel] = useState<
    "agent" | "changes" | "verification"
  >("agent");
  const [analysisSymbol, setAnalysisSymbol] = useState<string | null>(null);
  const [analysisMode, setAnalysisMode] = useState<"impact" | "references">(
    "impact",
  );
  const [analysisReady, setAnalysisReady] = useState(false);
  const [analysisError, setAnalysisError] = useState<string | null>(null);
  const [analysisAttempt, setAnalysisAttempt] = useState(0);
  const [theiaReportOpen, setTheiaReportOpen] = useState(false);
  const [theiaPlanOpen, setTheiaPlanOpen] = useState(false);
  const [worktreesOpen, setWorktreesOpen] = useState(false);
  const {
    theiaActiveFile,
    setTheiaActiveFile,
    theiaActiveSymbol,
    setTheiaActiveSymbol,
    theiaPreviewInput,
    setTheiaPreviewInput,
    previewUrls,
    setPreviewUrls,
    theiaPreviewOpen,
    setTheiaPreviewOpen,
    theiaAgentOpen,
    setTheiaAgentOpen,
    agentWidth,
    agentMaximumWidth,
    resetAgentWidth,
    theiaDirtyCount,
    setTheiaDirtyCount,
    theiaPreviewRequest,
    setTheiaPreviewRequest,
    theiaOpenFileRequest,
    theiaFrameRef,
    theiaPreviewRef,
    workbenchRef,
    onDetectedPreviewUrl,
    openTheiaFile,
    startAgentResize,
    resizeAgentByKey,
  } = useTheiaWorkbenchBridge(cwd, messages, onToast);
  const visibleDirtyCount = theiaDirtyCount ?? 0;
  const [theiaVerificationOutput, setTheiaVerificationOutput] = useState("");
  const [selectedVerification, setSelectedVerification] =
    useState<VerificationRecord | null>(null);
  const [contextPaths, setContextPaths] = useState<string[]>([]);
  const [modelId, setModelId] = useState(defaultModelId);
  const [intent, setIntent] = useState<CodingIntent>("agent");
  const [includeEditor, setIncludeEditor] = useState(true);
  const [editorContext, setEditorContext] =
    useState<EditorContextSnapshot | null>(null);
  const [extraContext, setExtraContext] = useState<
    Array<{ id: string; label: string; text: string }>
  >([]);
  const [contextPreview, setContextPreview] = useState<string | null>(null);
  const [diagnosticPicker, setDiagnosticPicker] = useState<
    EditorContextSnapshot["diagnostics"] | null
  >(null);
  const [planVisible, setPlanVisible] = useState(false);
  const [completionEnabled, setCompletionEnabled] = useState(
    () => localStorage.getItem("echo-coding-tab-enabled") !== "false",
  );
  const [completionStatus, setCompletionStatus] = useState("Tab 已启用");
  useEffect(() => {
    localStorage.setItem("echo-coding-tab-enabled", String(completionEnabled));
  }, [completionEnabled]);

  const [startError, setStartError] = useState<string | null>(null);
  const [starting, setStarting] = useState(false);
  const [sending, setSending] = useState(false);
  const [phaseReason, setPhaseReason] = useState<string>();
  const [blocker, setBlocker] = useState<string | null | undefined>(undefined);
  const [busyPath, setBusyPath] = useState<string | null>(null);
  const [committing, setCommitting] = useState(false);
  const [integrating, setIntegrating] = useState(false);
  const [redRunning, setRedRunning] = useState(false);
  const [redRunId, setRedRunId] = useState<string | null>(null);
  const [tddEvidence, setTddEvidence] = useState<TddEvidence | null>(null);
  const [isolatedWorkspace, setIsolatedWorkspace] =
    useState<IsolatedWorkspace | null>(null);
  const [creatingIsolatedWorkspace, setCreatingIsolatedWorkspace] =
    useState(false);
  const [projectLabels, setProjectLabels] = useState<Record<string, string>>(
    {},
  );
  const [detected, setDetected] = useState<DetectedCommand[]>([]);
  const [detectedReady, setDetectedReady] = useState(false);
  const [commandOutput, setCommandOutput] = useState("");
  const [reportRevision, setReportRevision] = useState(0);
  const [deliveryReport, setDeliveryReport] = useState<DeliveryReport | null>(
    null,
  );
  const {
    prepareManualMutation: prepareManualMutationUnfenced,
    finishManualMutation,
    blockInterruptedManualMutation,
  } = useCodingMutationLifecycle(cwd, onToast, setReportRevision);
  const prepareManualMutation = useCallback(async () => {
    assertCodingWorkspaceCancellationClear(cwd);
    const ticket = await prepareManualMutationUnfenced();
    assertCodingWorkspaceCancellationClear(cwd);
    return ticket;
  }, [cwd, prepareManualMutationUnfenced]);
  const contextSaveQueueRef = useRef<Promise<void>>(Promise.resolve());
  const previousContextScopeRef = useRef<string | null>(null);

  const task = useTaskStore((state) => state.task);
  const intentKey = `echo-coding-intent-v1:${cwd}:${task?.id ?? "new"}`;
  useEffect(() => {
    const saved = localStorage.getItem(intentKey);
    setIntent(saved === "ask" || saved === "plan" ? saved : "agent");
    setEditorContext(null);
    setExtraContext([]);
    setContextPreview(null);
    setDiagnosticPicker(null);
    setSelectedVerification(null);
    setIncludeEditor(true);
    setPlanVisible(saved === "plan");
  }, [intentKey]);
  const chooseIntent = (next: CodingIntent) => {
    setIntent(next);
    localStorage.setItem(intentKey, next);
    if (next === "plan") setPlanVisible(true);
  };
  const captureContext = useCallback(
    async (kind: "document" | "diagnostics" | "terminal" = "document") => {
      const snapshot = await theiaFrameRef.current?.getContext();
      if (!snapshot || !isWorkspaceCurrent())
        throw new Error("IDE 尚未连接，无法读取当前上下文");
      setEditorContext(snapshot);
      if (kind === "document") {
        setIncludeEditor(true);
        setContextPreview(JSON.stringify(snapshot.document, null, 2));
      } else {
        const value =
          kind === "diagnostics" ? snapshot.diagnostics : snapshot.terminal;
        if (!value || (Array.isArray(value) && !value.length)) {
          onToast?.(
            kind === "terminal" ? "请在终端选择输出后重试" : "当前没有诊断",
          );
          return;
        }
        if (kind === "diagnostics") {
          setDiagnosticPicker(snapshot.diagnostics);
          return;
        }
        const text = JSON.stringify(value);
        setExtraContext((current) => [
          ...current.filter((item) => item.id !== kind),
          {
            id: kind,
            label: kind === "terminal" ? "终端输出" : "Problems",
            text,
          },
        ]);
        setContextPreview(JSON.stringify(value, null, 2));
      }
    },
    [isWorkspaceCurrent, onToast, theiaFrameRef],
  );
  const contextForTurn = useCallback(async () => {
    const requested = includeEditor && theiaDirtyCount !== null;
    const snapshot = requested
      ? await theiaFrameRef.current?.getContext()
      : null;
    if (requested && !snapshot)
      throw new Error("编辑器上下文读取失败，请重试或移除该上下文");
    if (snapshot) {
      setEditorContext(snapshot);
      if (snapshot.document) setTheiaActiveFile(snapshot.document.path);
    }
    return JSON.stringify({
      workspace: cwd,
      document: snapshot?.document ?? null,
      openDocuments:
        snapshot?.openDocuments
          .filter((document) => document.dirty)
          .map((document) => ({
            ...document,
            content: document.content.slice(0, 8000),
            truncated: document.truncated || document.content.length > 8000,
          })) ?? [],
      extra: extraContext,
      truncated: snapshot?.truncated ?? false,
    });
  }, [cwd, includeEditor, extraContext, theiaFrameRef, theiaDirtyCount]);
  const isTaskCurrent = useCodingActionScope(cwd, task?.id);
  useEffect(() => {
    setSending(false);
  }, [task?.id]);
  const lifecycleSettling = useCodingRuntimeStore((state) =>
    task ? Boolean(state.settling[codingRuntimeKey(cwd, task.id)]) : false,
  );
  const stopping = useCodingRuntimeStore((state) =>
    task ? Boolean(state.stopping[codingRuntimeKey(cwd, task.id)]) : false,
  );
  const documentCancellationPending = useCodingRuntimeStore(state => codingWorkspaceCancellationPending(cwd, state));
  const cancellationChecking = useCodingRuntimeStore(state => !state.documentCancellationReady);
  const documentCancellationBlocked = documentCancellationPending || cancellationChecking;
  const cancellationMessage = documentCancellationPending
    ? streaming ? "正在停止，等待 Runtime、编辑器或外部工具确认；已发生修改保留。" : DOCUMENT_CANCELLATION_PENDING_MESSAGE
    : cancellationChecking ? DOCUMENT_CANCELLATION_CHECKING_MESSAGE : undefined;
  const backgroundRunId = useCodingRuntimeStore((state) =>
    task ? state.activeRunIds[codingRuntimeKey(cwd, task.id)] : undefined,
  );
  const summaries = useTaskStore((state) => state.summaries);
  useEffect(() => {
    if (!cwd || !task) {
      setTddEvidence(null);
      return;
    }
    let disposed = false;
    void codingApi
      .tddStatus(cwd, task.id)
      .then((evidence) => {
        if (!disposed) setTddEvidence(evidence);
      })
      .catch(() => {
        if (!disposed) setTddEvidence(null);
      });
    return () => {
      disposed = true;
    };
  }, [cwd, task?.id, task?.phase, task?.updatedAt]);
  const recentProjectsKey = recentCodingProjects
    .map((project) => project.cwd)
    .join("\0");
  const summariesKey = summaries
    .map((summary) => `${summary.id}:${summary.name}`)
    .join("\0");
  useEffect(() => {
    let disposed = false;
    void Promise.all(
      recentCodingProjects.map(async (project) => {
        const workspace = await codingApi
          .isolatedWorkspaceInfo(project.cwd)
          .catch(() => null);
        if (!workspace) return null;
        const tasks = await codingApi.listTasks(project.cwd).catch(() => []);
        const sourceName =
          workspace.sourceRoot
            .replace(/\\/g, "/")
            .split("/")
            .filter(Boolean)
            .pop() ?? "项目";
        const taskName = tasks[0]?.name ?? "并行任务";
        return [project.cwd, `${sourceName} · ${taskName}`] as const;
      }),
    ).then((entries) => {
      if (!disposed) {
        const labels: Record<string, string> = {};
        for (const entry of entries) if (entry) labels[entry[0]] = entry[1];
        setProjectLabels(labels);
      }
    });
    return () => {
      disposed = true;
    };
  }, [recentProjectsKey, summariesKey]);
  const changeSet = useTaskStore((state) => state.changeSet);
  const problems = useTaskStore((state) => state.problems);
  const ledger = useTaskStore((state) => state.ledger);
  const verifications = useTaskStore((state) => state.verifications);
  const [freshness, setFreshness] = useState<{
    contentRevision: string;
    configurationRevision: string;
  } | null>(null);
  useEffect(() => {
    let disposed = false;
    setFreshness(null);
    if (task)
      void codingApi
        .verificationFreshness(cwd, task.id)
        .then((value) => {
          if (!disposed) setFreshness(value);
        })
        .catch(() => undefined);
    return () => {
      disposed = true;
    };
  }, [
    cwd,
    task?.id,
    task?.updatedAt,
    changeSet?.contentRevision,
    reportRevision,
    theiaDirtyCount,
  ]);

  const orchestrator = useTaskStore((state) => state.orchestrator);
  const activeTaskCount = useMemo(() => {
    const activeIds = new Set(
      summaries
        .filter((summary) => isBusyPhase(summary.phase))
        .map((summary) => summary.id),
    );
    // The detail record can be newer than the summary list immediately after
    // a phase transition. Count it as well so navigation can never create a
    // brief window in which an active task is accidentally abandoned.
    if (task && isBusyPhase(task.phase)) activeIds.add(task.id);
    return activeIds.size;
  }, [summaries, task]);

  useEffect(() => {
    if (!cwd) {
      setIsolatedWorkspace(null);
      return;
    }
    let disposed = false;
    void codingApi
      .isolatedWorkspaceInfo(cwd)
      .then((workspace) => {
        if (!disposed) setIsolatedWorkspace(workspace);
      })
      .catch(() => {
        if (!disposed) setIsolatedWorkspace(null);
      });
    return () => {
      disposed = true;
    };
  }, [cwd]);
  const verificationCommands = useMemo(
    () => mergeTaskVerificationCommands(detected, task),
    [detected, task],
  );
  const { activeRunId, activeRunIdRef, runningVerification, runVerifications } =
    useVerificationRunner({
      cwd,
      task,
      commands: verificationCommands,
      detectedReady,
      setCommandOutput,
      requestConfirmation: requestTaskConfirmation,
      prepareEditor,
      onToast,
    });

  useEffect(() => {
    if (runningVerification) setTheiaVerificationOutput("");
  }, [runningVerification]);
  useEffect(() => {
    setTheiaReportOpen(false);
    setTheiaPlanOpen(false);
    setAnalysisSymbol(null);
    setWorktreesOpen(false);
  }, [cwd, task?.id]);

  useEffect(() => { setTheiaActiveSymbol(null); setTheiaActiveFile(null); }, [cwd]);

  useEffect(() => {
    setPreviewUrls([]);
    setTheiaPreviewInput("");
  }, [cwd]);

  useEffect(() => {
    if (!analysisSymbol || !cwd) return;
    let cancelled = false;
    let acquired = false;
    setAnalysisReady(false);
    setAnalysisError(null);
    void codingApi
      .indexBootstrap(cwd)
      .then(() => {
        acquired = true;
        if (!cancelled) setAnalysisReady(true);
        else void codingApi.indexRelease(cwd);
      })
      .catch((error) => {
        if (!cancelled)
          setAnalysisError(String(error).replace(/^Error:\s*/, ""));
      });
    return () => {
      cancelled = true;
      if (acquired) void codingApi.indexRelease(cwd);
    };
  }, [analysisAttempt, analysisSymbol, cwd]);

  useEffect(() => {
    setPhaseReason(undefined);
    setBlocker(undefined);
  }, [task?.id]);

  useEffect(() => setModelId(defaultModelId), [defaultModelId]);

  // Bind the task store to this workspace and load its task list.
  useEffect(() => {
    useTaskStore.getState().setRoot(cwd);
    if (cwd) void useTaskStore.getState().refreshSummaries();
  }, [cwd]);

  // Task context belongs to its task; draft context may be carried to an
  // isolated worktree after a parallel task is started from the composer.
  useEffect(() => {
    const scope = `${cwd}\0${task?.id ?? "draft"}`;
    const previousScope = previousContextScopeRef.current;
    previousContextScopeRef.current = scope;
    if (task) {
      setContextPaths(task.contextPaths ?? []);
      return;
    }
    if (!cwd) return;
    let paths: string[] = [];
    let restored = false;
    try {
      const key = codingTaskContextDraftKey(cwd);
      const stored = localStorage.getItem(key);
      if (stored) {
        const parsed: unknown = JSON.parse(stored);
        if (Array.isArray(parsed))
          paths = parsed.filter(
            (path): path is string => typeof path === "string",
          );
        localStorage.removeItem(key);
        restored = true;
      }
    } catch {
      // Draft context is optional; never block opening the workspace.
    }
    if (restored || (previousScope !== null && previousScope !== scope))
      setContextPaths(paths);
  }, [cwd, task?.id]);

  /**
   * The orchestrator is the only authority on phase, so the UI reacts to its
   * events instead of inferring progress from the message stream.
   */
  useEffect(() => {
    if (!cwd) return;
    let disposed = false;
    const unlisteners: Array<() => void> = [];
    void onPhaseChanged((event) => {
      if (disposed || event.root !== cwd) return;
      useTaskStore.getState().applyPhase(event.taskId, event.phase);
      if (useTaskStore.getState().task?.id === event.taskId) {
        setPhaseReason(event.reason);
        setBlocker(event.blocker ?? null);
      }
      setReportRevision((value) => value + 1);
      void useTaskStore.getState().refreshTaskState();
    }).then((unlisten) => {
      if (disposed) unlisten();
      else unlisteners.push(unlisten);
    });
    void onVerificationUpdated((record) => {
      if (disposed || useTaskStore.getState().task?.id !== record.taskId)
        return;
      useTaskStore.getState().applyVerification(record);
    }).then((unlisten) => {
      if (disposed) unlisten();
      else unlisteners.push(unlisten);
    });
    void onVerificationOutput((event) => {
      if (
        disposed ||
        event.root !== cwd ||
        useTaskStore.getState().task?.id !== event.taskId ||
        (activeRunIdRef.current !== event.runId &&
          useCodingRuntimeStore.getState().activeRunIds[
            codingRuntimeKey(cwd, event.taskId)
          ] !== event.runId)
      )
        return;
      // Cap retained output so a verbose build cannot grow the renderer's memory.
      setCommandOutput((current) => {
        const next = current + event.chunk;
        return next.length > 400_000
          ? `…较早输出已省略…\n${next.slice(-400_000)}`
          : next;
      });
    }).then((unlisten) => {
      if (disposed) unlisten();
      else unlisteners.push(unlisten);
    });
    return () => {
      disposed = true;
      for (const unlisten of unlisteners) unlisten();
    };
  }, [cwd]);

  // Re-detect at phase boundaries as well as workspace open. An implementation
  // may add/remove package scripts or manifests, so a cached command list is
  // not valid evidence for the new source tree.
  useEffect(() => {
    if (!cwd) {
      setDetected([]);
      setDetectedReady(false);
      return;
    }
    let cancelled = false;
    setDetectedReady(false);
    void codingApi
      .detectCommands(cwd)
      .then((commands) => {
        if (!cancelled) setDetected(commands);
      })
      .catch(() => {
        if (!cancelled) setDetected([]);
      })
      .finally(() => {
        if (!cancelled) setDetectedReady(true);
      });
    return () => {
      cancelled = true;
    };
  }, [cwd, task?.phase, task?.phase === "verifying" ? task.updatedAt : null]);

  const saveTheiaBeforeLeaving = useCallback(async (): Promise<boolean> => {
    if (!cwd) return true;
    let dirtyCount: number;
    try {
      dirtyCount = (await theiaFrameRef.current?.getDirtyCount()) ?? -1;
    } catch (error) {
      return confirmTaskAction({
        title: "无法确认 IDE 文件状态",
        description: `${String(error).replace(/^Error:\s*/, "")}。继续离开可能丢失未保存的编辑内容。`,
        confirmLabel: "仍要离开",
        danger: true,
      });
    }
    if (dirtyCount < 0) return false;
    setTheiaDirtyCount(dirtyCount);
    if (dirtyCount === 0) return true;
    const confirmed = await confirmTaskAction({
      title: `保存 ${dirtyCount} 个文件后离开？`,
      description:
        "当前 IDE 有未保存的编辑内容。保存成功后继续，保存失败时会留在当前项目。",
      confirmLabel: "保存并继续",
    });
    if (!confirmed) return false;
    try {
      const saved = await theiaFrameRef.current?.saveAll();
      if (saved) return true;
      return confirmTaskAction({
        title: "仍有文件未保存",
        description: "IDE 中仍有未保存的文件。继续离开会放弃这些编辑内容。",
        confirmLabel: "放弃并离开",
        danger: true,
      });
    } catch (error) {
      return confirmTaskAction({
        title: "IDE 文件保存失败",
        description: `${String(error).replace(/^Error:\s*/, "")}。继续离开会放弃未保存的编辑内容。`,
        confirmLabel: "放弃并离开",
        danger: true,
      });
    }
  }, [confirmTaskAction, cwd]);

  useEffect(() => {
    onRegisterLeaveGuard?.(saveTheiaBeforeLeaving);
    return () => onRegisterLeaveGuard?.(null);
  }, [onRegisterLeaveGuard, saveTheiaBeforeLeaving]);

  const exitSafely = useCallback(async () => {
    if (!(await saveTheiaBeforeLeaving())) return;
    onExit?.();
  }, [onExit, saveTheiaBeforeLeaving]);

  const switchProject = useCallback(
    async (nextCwd: string) => {
      if (!nextCwd || nextCwd === cwd) return;
      if (cwd && !(await saveTheiaBeforeLeaving())) return;
      onSelectWorkspace?.(nextCwd);
    },
    [cwd, onSelectWorkspace, saveTheiaBeforeLeaving],
  );

  const pickWorkspace = useCallback(async () => {
    try {
      const picked = await filesystemPickDirectory();
      if (picked) await switchProject(picked);
    } catch (error) {
      onToast?.(`打开文件夹失败：${String(error).replace(/^Error:\s*/, "")}`);
    }
  }, [onToast, switchProject]);

  const openAnotherProject = useCallback(() => {
    void pickWorkspace();
  }, [pickWorkspace]);

  const removeRecentProject = useCallback(
    async (projectCwd: string) => {
      const active = await codingApi
        .listTasks(projectCwd)
        .then((items) => items.some((item) => isBusyPhase(item.phase)))
        .catch(() => true);
      if (active) {
        onToast?.("这个项目仍有任务在执行；任务完成或停止后才能从最近项目移除");
        return;
      }
      if (projectCwd === cwd && !(await saveTheiaBeforeLeaving())) return;
      onCloseCodingWorkspace?.(projectCwd);
    },
    [cwd, onCloseCodingWorkspace, onToast, saveTheiaBeforeLeaving],
  );

  useEffect(() => {
    const beforeUnload = (event: BeforeUnloadEvent) => {
      const dirty =
        Boolean(cwd) && (theiaDirtyCount === null || theiaDirtyCount > 0);
      if (!dirty && activeTaskCount === 0) return;
      event.preventDefault();
      event.returnValue = "";
    };
    window.addEventListener("beforeunload", beforeUnload);
    return () => window.removeEventListener("beforeunload", beforeUnload);
  }, [activeTaskCount, cwd, theiaDirtyCount]);

  /** Derive a task name from the requirement's first clause. */
  const deriveName = useCallback((requirement: string) => {
    const firstLine =
      requirement.split(/[\n。；;]/)[0]?.trim() ?? requirement.trim();
    return firstLine.length > 24
      ? `${firstLine.slice(0, 24)}…`
      : firstLine || "开发任务";
  }, []);

  const openParallelWorkspace = useCallback(
    async (draft?: string, images: string[] = []) => {
      if (creatingIsolatedWorkspace || !cwd) return false;
      if (!onSelectWorkspace) {
        setStartError("无法打开隔离工作树，请重新进入代码开发页面");
        return false;
      }
      if (!(await saveTheiaBeforeLeaving())) return false;
      setCreatingIsolatedWorkspace(true);
      try {
        const workspace = await codingApi.createIsolatedWorkspace(cwd);
        if (draft) {
          try {
            localStorage.setItem(codingTaskDraftKey(workspace.root), draft);
            localStorage.setItem(
              `${codingTaskDraftKey(workspace.root)}:images`,
              JSON.stringify(images.map((path) => ({ path, preview: "" }))),
            );
            localStorage.setItem(
              `echo-coding-intent-v1:${workspace.root}:new`,
              intent,
            );
            if (contextPaths.length > 0) {
              localStorage.setItem(
                codingTaskContextDraftKey(workspace.root),
                JSON.stringify(contextPaths),
              );
            }
          } catch {
            // The current editor keeps the text if workspace switching fails.
          }
        }
        onSelectWorkspace(workspace.root);
        onToast?.("已创建隔离工作树；新任务可以与原任务并行执行");
        return true;
      } catch (error) {
        const reason = String(error).replace(/^Error:\s*/, "");
        setStartError(`无法创建并行任务：${reason}`);
        onToast?.(`无法创建并行任务：${reason}`);
        return false;
      } finally {
        setCreatingIsolatedWorkspace(false);
      }
    },
    [
      contextPaths,
      intent,
      creatingIsolatedWorkspace,
      cwd,
      onSelectWorkspace,
      onToast,
      saveTheiaBeforeLeaving,
    ],
  );

  /**
   * Create the task, then hand the requirement to the host so it can open an
   * Agent session. The orchestrator records the phase transition; the workbench
   * never decides it locally.
   */
  const startTask = useCallback(
    async (
      requirement: string,
      _documentationTarget?: unknown,
      additionalContextPaths: string[] = [],
      attachments: string[] = [],
    ) => {
      if (!cwd || !onStartRun) return false;
      if (codingWorkspaceCancellationBlocked(cwd)) { setStartError(cancellationMessage ?? DOCUMENT_CANCELLATION_PENDING_MESSAGE); return false; }
      const trimmedRequirement = requirement.trim();
      if (!trimmedRequirement) {
        setStartError("请输入任务目标后再开始");
        return false;
      }
      const generation = ++startGeneration.current;
      const isCurrent = () =>
        isWorkspaceCurrent() &&
        generation === startGeneration.current &&
        useTaskStore.getState().root === cwd;
      const assertCurrent = () => {
        assertCodingWorkspaceCancellationClear(cwd);
        if (!isCurrent())
          throw new EditorPreparationCancelled("项目已切换，启动已取消");
      };
      setStarting(true);
      setStartError(null);
      let createdId: string | undefined;
      try {
        const editorContextText = await contextForTurn();
        assertCurrent();
        // Summaries and isolation details load asynchronously. Check the host
        // at submit time so an early click cannot start work in a busy tree.
        const [latestTasks, latestIsolation] = await Promise.all([
          codingApi.listTasks(cwd),
          codingApi.isolatedWorkspaceInfo(cwd),
        ]);
        assertCurrent();
        if (
          latestTasks.some((item) => isBusyPhase(item.phase)) ||
          latestIsolation
        ) {
          return await openParallelWorkspace(trimmedRequirement, attachments);
        }
        const effectiveContextPaths = [
          ...new Set(
            [...contextPaths, ...additionalContextPaths]
              .map((path) => workspaceRelativePath(cwd, path))
              .filter(Boolean),
          ),
        ];
        const created = await useTaskStore
          .getState()
          .createTask(deriveName(trimmedRequirement), trimmedRequirement, cwd);
        if (!created) {
          if (isCurrent())
            setStartError(useTaskStore.getState().error ?? "创建任务失败");
          return false;
        }
        createdId = created.id;
        localStorage.setItem(
          `echo-coding-intent-v1:${cwd}:${created.id}`,
          intent,
        );
        setIntent(intent);
        assertCurrent();
        try {
          localStorage.removeItem(codingTaskDraftKey(cwd));
        } catch {
          // Task creation succeeded; draft cleanup is best-effort.
        }
        await codingApi.setTaskContext(cwd, created.id, effectiveContextPaths);
        assertCurrent();
        // Persist a task-start checkpoint before the Agent can touch the
        // workspace. Git repositories get HEAD protection; ordinary folders
        // get an application-owned filesystem checkpoint.
        await codingApi.captureBaseline(cwd, created.id, []);
        assertCurrent();
        await codingApi.submitRequirement(cwd, created.id);
        assertCurrent();
        let boundSession: string | undefined;
        const bindSession = async (sessionId: string) => {
          if (boundSession && boundSession !== sessionId) {
            throw new Error("任务收到了不一致的 Agent 会话标识");
          }
          await codingApi.bindTaskRuntime(
            cwd,
            created.id,
            sessionId,
            modelId ?? "",
          );
          useCodingRuntimeStore.getState().track(cwd, created.id);
          if (isCurrent() && useTaskStore.getState().task?.id === created.id)
            await useTaskStore.getState().selectTask(created.id);
          boundSession = sessionId;
        };
        const session = await onStartRun(
          cwd,
          trimmedRequirement,
          modelId,
          effectiveContextPaths,
          bindSession,
          buildCodingWorkflowPrompt(
            trimmedRequirement,
            effectiveContextPaths,
            false,
            { intent, editorContext: editorContextText },
          ),
          {
            mode: intent,
            attachments,
            documentToolBridge: true,
            workspace: cwd,
            taskId: created.id,
          },
        );
        if (!session) {
          const reason = "未能启动 Agent 会话，任务已停止且不会继续执行";
          await codingApi
            .reportStartFailed(cwd, created.id, reason)
            .catch(() => undefined);
          if (isCurrent()) {
            setStartError(reason);
            await useTaskStore.getState().refreshTaskState();
          }
          return false;
        }
        if (boundSession !== session) {
          throw new Error("Agent 返回的会话与已绑定会话不一致");
        }
        return true;
      } catch (error) {
        const reason = String(error).replace(/^Error:\s*/, "");
        if (createdId) {
          await codingApi
            .reportStartFailed(cwd, createdId, reason)
            .catch(() => undefined);
          if (isCurrent())
            await useTaskStore
              .getState()
              .refreshTaskState()
              .catch(() => undefined);
        }
        if (isCurrent() && !(error instanceof EditorPreparationCancelled))
          setStartError(reason);
        return false;
      } finally {
        if (isCurrent()) setStarting(false);
      }
    },
    [
      contextPaths,
      cwd,
      deriveName,
      isWorkspaceCurrent,
      modelId,
      onStartRun,
      openParallelWorkspace,
      contextForTurn,
      intent,
      cancellationMessage,
    ],
  );

  const sendFollowup = useCallback(
    async (
      text: string,
      mutating = true,
      managedPrompt?: string,
      attachments: string[] = [],
      turnIntent: CodingIntent = intent,
      review?: CodingTurnOptions["review"],
    ) => {
      if (!onSendMessage) return false;
      if (codingWorkspaceCancellationBlocked(cwd)) { onToast?.(cancellationMessage ?? DOCUMENT_CANCELLATION_PENDING_MESSAGE); return false; }
      if (
        !task?.sessionId ||
        activeSessionId !== task.sessionId ||
        hostSessionId !== task.sessionId
      ) {
        onToast?.("当前任务暂时无法连接，请重新选择或新建任务");
        return false;
      }
      const releaseAdmission = claimCodingAdmission(cwd, task.id);
      if (!releaseAdmission) return false;
      setSending(true);
      let reopened = false;
      try {
        const editorContextText = await contextForTurn();
        assertCodingWorkspaceCancellationClear(cwd);
        if (!isTaskCurrent()) return false;
        if (mutating && task) {
          if (
            ["paused", "stopped", "blocked", "delivered"].includes(task.phase)
          ) {
            if (!cwd) return false;
            await codingApi.beginFollowup(cwd, task.id, text);
            reopened = true;
            if (!isTaskCurrent())
              throw new EditorPreparationCancelled(
                "任务已切换，补充要求未发送",
              );
            await useTaskStore.getState().refreshTaskState();
            assertCodingWorkspaceCancellationClear(cwd);
            if (!isTaskCurrent())
              throw new EditorPreparationCancelled(
                "任务已切换，补充要求未发送",
              );
          } else if (
            !["discovering", "implementing", "repairing"].includes(task.phase)
          ) {
            onToast?.(
              task.phase === "verifying"
                ? "正在验证当前改动，请等待验证结束后再补充开发要求"
                : "当前任务阶段不能修改代码",
            );
            return false;
          }
        }
        const effectiveContextPaths = [...new Set(contextPaths)];
        const promptTextOverride =
          managedPrompt ??
          (task
            ? buildCodingWorkflowPrompt(text, effectiveContextPaths, true, {
                intent: turnIntent,
                editorContext: editorContextText,
              })
            : undefined);
        if (!isTaskCurrent()) return false;
        assertCodingWorkspaceCancellationClear(cwd);
        const accepted = await onSendMessage(text, promptTextOverride, {
          mode: turnIntent,
          attachments,
          documentToolBridge: true,
          workspace: cwd,
          taskId: task.id,
          review,
        });
        if (accepted === false && reopened) {
          await blockInterruptedManualMutation(
            { taskId: task.id, closeRound: true },
            "Agent 未接收补充要求",
          );
        }
        return accepted !== false;
      } catch (error) {
        const message = String(error).replace(/^Error:\s*/, "");
        if (reopened) {
          await blockInterruptedManualMutation(
            { taskId: task.id, closeRound: true },
            `Agent 补充要求发送失败：${message}`,
          );
        }
        onToast?.(`无法继续当前任务：${message}`);
        return false;
      } finally {
        releaseAdmission();
        if (isTaskCurrent()) setSending(false);
      }
    },
    [
      activeSessionId,
      blockInterruptedManualMutation,
      contextPaths,
      cwd,
      hostSessionId,
      onSendMessage,
      onToast,
      contextForTurn,
      intent,
      isTaskCurrent,
      task,
      cancellationMessage,
    ],
  );

  const requestCodeReview = useCallback(async () => {
    if (!task || streaming || sending) return;
    try {
      await prepareEditor();
      if (!isTaskCurrent()) return;
      const freshness = await codingApi.verificationFreshness(cwd, task.id);
      const set = await codingApi.getChangeSet(cwd, task.id);
      if (!set.changes.length) {
        onToast?.("当前没有待审查的修改");
        return;
      }
      const diffs = await Promise.all(
        set.changes.slice(0, 30).map(async (change) => ({
          path: change.path,
          ...(await codingApi.changeDiff(cwd, task.id, change.path)),
        })),
      );
      const source = JSON.stringify(diffs);
      if (source.length > 128000 || set.changes.length > 30)
        throw new Error("当前差异超过单轮审查预算，请先分批提交或指定范围");
      const latest = await codingApi.verificationFreshness(cwd, task.id);
      if (latest.contentRevision !== freshness.contentRevision)
        throw new Error("差异已变化，请重新发起审查");
      await sendFollowup(
        `审查以下真实 Diff，指出具体问题、依据、文件和行号、严重程度，并给出可执行修复建议；没有问题时如实说明。不要修改工程。\nrevision=${freshness.contentRevision}\n${source}`,
        true,
        undefined,
        [],
        "ask",
        {
          taskId: task.id,
          kind: "code_quality",
          contentRevision: freshness.contentRevision,
        },
      );
    } catch (error) {
      onToast?.(`审查尚未开始：${String(error)}`);
    }
  }, [
    cwd,
    task,
    streaming,
    sending,
    onToast,
    sendFollowup,
    prepareEditor,
    isTaskCurrent,
  ]);

  const activateCodingTask = useCallback(
    async (taskId: string) => {
      setSending(true);
      try {
        await useTaskStore.getState().selectTask(taskId);
        setTheiaPanel("agent");
        const selected = useTaskStore.getState().task;
        setContextPaths(selected?.contextPaths ?? []);
        if (selected?.sessionId) {
          await onActivateSession?.(selected.sessionId, cwd);
          if (selected.modelId) setModelId(selected.modelId);
        }
      } catch (error) {
        onToast?.(
          `恢复任务会话失败：${String(error).replace(/^Error:\s*/, "")}`,
        );
      } finally {
        setSending(false);
      }
    },
    [cwd, onActivateSession, onToast],
  );

  const beginNewTask = useCallback(async () => {
    if (activeTaskCount > 0 || isolatedWorkspace) {
      await openParallelWorkspace();
      return;
    }
    useTaskStore.getState().reset(true);
    setContextPaths([]);
    setTheiaPanel("agent");
    setPhaseReason(undefined);
    setBlocker(undefined);
  }, [activeTaskCount, isolatedWorkspace, openParallelWorkspace]);

  const persistTaskContext = useCallback(
    (taskId: string, paths: string[]) => {
      // Tauri invocations may complete out of order. Serialize context writes so
      // rapid add/remove actions cannot resurrect an older selection on disk.
      contextSaveQueueRef.current = contextSaveQueueRef.current
        .catch(() => undefined)
        .then(async () => {
          await codingApi.setTaskContext(cwd, taskId, paths);
        })
        .catch((error) => {
          onToast?.(
            `保存任务上下文失败：${String(error).replace(/^Error:\s*/, "")}`,
          );
        });
    },
    [cwd, onToast],
  );

  /** SP3: «加入上下文» — push selected paths into `contextPaths`. */
  const addManyToContext = useCallback(
    (paths: string[]) => {
      if (paths.length === 0) return;
      setContextPaths((current) => {
        const normalized = paths
          .map((path) => workspaceRelativePath(cwd, path))
          .filter(Boolean);
        const next = [...new Set([...current, ...normalized])];
        if (task) {
          persistTaskContext(task.id, next);
        }
        return next;
      });
    },
    [cwd, onToast, persistTaskContext, task],
  );

  const removeFromContext = useCallback(
    (path: string) => {
      setContextPaths((current) => {
        const next = current.filter((entry) => entry !== path);
        if (task) {
          persistTaskContext(task.id, next);
        }
        return next;
      });
    },
    [persistTaskContext, task],
  );

  const renameCodingTask = useCallback(
    (summary: (typeof summaries)[number], returnFocus?: HTMLElement | null) => {
      requestTaskInput({
        title: "重命名开发任务",
        description:
          "任务名称只用于识别和切换，不会改变原始需求或 Agent 会话。",
        confirmLabel: "保存",
        returnFocus,
        fields: [
          {
            name: "name",
            label: "任务名称",
            defaultValue: summary.name,
            required: true,
            maxLength: 120,
          },
        ],
        validate: (values) =>
          values.name.trim() === summary.name
            ? "请输入与当前名称不同的新名称。"
            : null,
        action: async (values) => {
          await useTaskStore
            .getState()
            .renameTask(summary.id, values.name.trim());
        },
      });
    },
    [onToast, requestTaskInput, summaries],
  );

  const deleteCodingTask = useCallback(
    (summary: (typeof summaries)[number], returnFocus?: HTMLElement | null) => {
      if (isBusyPhase(summary.phase)) {
        onToast?.("任务正在执行或验证，请先停止后再删除");
        return;
      }
      const index = summaries.findIndex((entry) => entry.id === summary.id);
      const fallback = summaries[index + 1] ?? summaries[index - 1];
      const wasActive = task?.id === summary.id;
      requestTaskConfirmation({
        title: `删除开发任务「${summary.name}」？`,
        description: (
          <>
            将删除该任务的执行计划、差异基线、验证记录和交付报告。
            <br />
            工作区源码和绑定的 Agent 会话不会被删除。
          </>
        ),
        confirmLabel: "删除任务",
        danger: true,
        returnFocus,
        action: async () => {
          await useTaskStore.getState().deleteTask(summary.id);
          for (const key of Object.keys(
            useMessageQueueStore.getState().queues,
          )) {
            const owner = codingQueueOwner(key);
            if (owner?.root === cwd && owner.taskId === summary.id)
              useMessageQueueStore.getState().clear(key);
          }
          useCodingRuntimeStore.getState().untrack(cwd, summary.id);
          if (wasActive) {
            setPhaseReason(undefined);
            setBlocker(undefined);
            if (fallback) await activateCodingTask(fallback.id);
          }
        },
      });
    },
    [activateCodingTask, onToast, requestTaskConfirmation, summaries, task?.id],
  );

  const continueInterruptedTask = useCallback(async () => {
    if (
      !cwd ||
      !task ||
      (task.phase !== "paused" && task.phase !== "stopped") ||
      sending ||
      streaming ||
      lifecycleSettling || codingWorkspaceCancellationBlocked(cwd)
    )
      return;
    if (
      !onSendMessage ||
      !task.sessionId ||
      activeSessionId !== task.sessionId ||
      hostSessionId !== task.sessionId
    ) {
      onToast?.("当前任务尚未恢复连接，请重新选择后再试");
      return;
    }
    const interruptedAs = task.phase;
    let resumed = false;
    setSending(true);
    try {
      await prepareEditor();
      if (!isTaskCurrent()) return;
      assertCodingWorkspaceCancellationClear(cwd);
      const resumedTask = await codingApi.resumeTask(cwd, task.id);
      resumed = true;
      if (!isTaskCurrent())
        throw new EditorPreparationCancelled("项目已切换，继续执行已取消");
      await useTaskStore.getState().refreshTaskState();
      if (!isTaskCurrent())
        throw new EditorPreparationCancelled("项目已切换，继续执行已取消");
      const activeNode = resumedTask.taskNodes.find(
        (node) => node.status === "running",
      );
      const basePrompt = activeNode
        ? buildNodeContinuationInstruction(resumedTask, activeNode)
        : buildCodingWorkflowPrompt(
            `继续完成原始需求：${resumedTask.requirement}`,
            contextPaths,
            true,
          );
      const red =
        task.phaseReason === "等待红灯测试验证"
          ? await codingApi.tddStatus(cwd, task.id)
          : null;
      const prompt = red?.redRecordId
        ? `${basePrompt}\n\n测试先行 RED 已由原生命令 ${red.redCommand} 记录失败。现在实现最小代码使该测试转绿，随后运行完整验证；不要重写已经审阅的 RED 证据。`
        : red?.waiverReason
          ? `${basePrompt}\n\n本轮已记录测试先行不适用原因：${red.waiverReason}。按原始需求完成实现并运行适用验证，不要伪称已观察 RED。`
          : basePrompt;
      if (!isTaskCurrent())
        throw new EditorPreparationCancelled("项目已切换，继续执行已取消");
      assertCodingWorkspaceCancellationClear(cwd);
      const accepted = await onSendMessage("继续执行当前开发任务", prompt, {
        mode: intent,
        documentToolBridge: true,
        workspace: cwd,
        taskId: task.id,
      });
      if (accepted === false) throw new Error("Agent 未接收继续执行请求");
    } catch (error) {
      if (resumed) {
        await codingApi
          .reportInterrupted(cwd, task.id, interruptedAs)
          .catch(() => undefined);
        if (isTaskCurrent())
          await useTaskStore
            .getState()
            .refreshTaskState()
            .catch(() => undefined);
      }
      if (isTaskCurrent())
        onToast?.(
          `无法继续当前任务：${String(error).replace(/^Error:\s*/, "")}`,
        );
    } finally {
      if (isTaskCurrent()) setSending(false);
    }
  }, [
    activeSessionId,
    contextPaths,
    intent,
    cwd,
    hostSessionId,
    lifecycleSettling,
    onSendMessage,
    prepareEditor,
    isTaskCurrent,
    onToast,
    sending,
    streaming,
    task,
  ]);

  const codingQueueReady = Boolean(
    task?.sessionId &&
      activeSessionId === task.sessionId &&
      hostSessionId === task.sessionId &&
      apiReady &&
      !streaming &&
      !sending &&
      !lifecycleSettling &&
      !stopping &&
      !documentCancellationBlocked &&
      !transcript?.pendingSendNowPromptId &&
      !transcript?.planApprovals?.length &&
      !awaitingPermission &&
      !awaitingQuestion,
  );
  const codingQueueReadyRef = useRef(codingQueueReady);
  codingQueueReadyRef.current = codingQueueReady;
  const followupQueue = useCodingFollowupQueue({
    workspace: cwd,
    taskId: task?.id,
    sessionId: activeSessionId,
    autoReady: false,
    manualReady:
      codingQueueReady && !!task && MANUAL_FOLLOWUP_PHASES.includes(task.phase),
    send: (text, paths, promptId, manual, coding) => {
      const epoch =
        useCodingRuntimeStore.getState().actionEpochs[
          codingRuntimeKey(cwd, task!.id)
        ] ?? 0;
      return sendCodingQueuedFollowup({
        root: cwd,
        taskId: task!.id,
        sessionId: activeSessionId!,
        text,
        contextPaths: paths,
        promptId,
        manual,
        coding,
        canDispatch: () =>
          isTaskCurrent() &&
          codingQueueReadyRef.current &&
          !codingWorkspaceCancellationBlocked(cwd) &&
          !useCodingRuntimeStore.getState().stopping[
            codingRuntimeKey(cwd, task!.id)
          ] &&
          epoch ===
            (useCodingRuntimeStore.getState().actionEpochs[
              codingRuntimeKey(cwd, task!.id)
            ] ?? 0),
        onChanged: () => useTaskStore.getState().refreshTaskState(),
      });
    },
    onToast,
  });

  const stopActiveTask = useCallback(async () => {
    if (
      !cwd ||
      !task ||
      !isBusyPhase(task.phase) ||
      useCodingRuntimeStore.getState().stopping[codingRuntimeKey(cwd, task.id)]
    )
      return;
    useCodingRuntimeStore.getState().setStopping(cwd, task.id, true);
    // Stop also fences pending requirements, even if a verification completion
    // reaches the renderer before the native cancellation acknowledgement.
    const queueId = codingQueueId(cwd, task.id, task.sessionId);
    if (queueId) {
      const queue = useMessageQueueStore.getState();
      for (const item of queue.getQueue(queueId)) {
        if (
          item.status === "queued" &&
          !queue.setStatus(queueId, item.id, "paused")
        ) {
          onToast?.("任务停止已请求，但队列暂停未能保存，请检查本机存储后重试");
        }
      }
    }
    let awaitingSettlement = false;
    try {
      if (activeRunId || backgroundRunId)
        await codingApi.cancelVerification(activeRunId ?? backgroundRunId!);
      const cancelled = await onCancelRun?.();
      if (streaming && cancelled === false) return;
      awaitingSettlement = streaming && Boolean(onCancelRun);
      // A streaming turn is finalized by useTaskLifecycle when its stream
      // falls. Scheduler/verification phases without a stream need an explicit
      // persisted interruption so the stop control always has an effect.
      if (!streaming) {
        await codingApi.reportInterrupted(cwd, task.id, "stopped");
        await useTaskStore.getState().refreshTaskState();
      }
    } catch (error) {
      onToast?.(`停止任务失败：${String(error).replace(/^Error:\s*/, "")}`);
    } finally {
      if (!awaitingSettlement)
        useCodingRuntimeStore.getState().setStopping(cwd, task.id, false);
    }
  }, [
    activeRunId,
    backgroundRunId,
    cwd,
    onCancelRun,
    onToast,
    streaming,
    task,
  ]);

  const rollbackTask = useCallback(async () => {
    if (!cwd || !task || codingWorkspaceCancellationBlocked(cwd)) return;
    const count = changeSet?.changes.length ?? 0;
    const confirmed = await confirmTaskAction({
      title: `回滚当前任务的 ${count} 个文件？`,
      description:
        "将精确恢复任务开始时的内容。IDE 会在文件系统事件后刷新对应文件。",
      confirmLabel: "回滚任务",
      danger: true,
    });
    if (!confirmed) return;
    try {
      await prepareEditor();
      if (!isWorkspaceCurrent()) return;
      const restored = await withWorkspaceDiskMutation(cwd, () =>
        { assertCodingWorkspaceCancellationClear(cwd); return codingApi.rollbackTask(cwd, task.id); },
      );
      await useTaskStore.getState().refreshTaskState();
      onToast?.(`已回滚 ${restored.length} 个文件`);
    } catch (error) {
      onToast?.(`回滚失败：${String(error).replace(/^Error:\s*/, "")}`);
    }
  }, [
    changeSet?.changes.length,
    confirmTaskAction,
    cwd,
    isWorkspaceCurrent,
    onToast,
    prepareEditor,
    task,
  ]);

  const discardChange = useCallback(
    async (path: string) => {
      if (!cwd || !task || codingWorkspaceCancellationBlocked(cwd)) return;
      const confirmed = await confirmTaskAction({
        title: `丢弃 ${path} 的全部改动？`,
        description:
          "普通文本恢复会保留编辑器撤销记录；新建、删除及文件属性变化使用真实文件操作恢复，不加入文本撤销栈。",
        confirmLabel: "丢弃改动",
        danger: true,
      });
      if (!confirmed) return;
      try {
        await prepareEditor();
      } catch (error) {
        onToast?.(String(error).replace(/^Error:\s*/, ""));
        return;
      }
      if (!isTaskCurrent()) return;
      setBusyPath(path);
      let mutation: TheiaMutationTicket | null = null;
      try {
        const diff = await codingApi.changeDiff(cwd, task.id, path);
        if (!isTaskCurrent()) return;
        if (diff.canApplyText === true && !diff.binary) {
          const editor = theiaFrameRef.current;
          if (!editor) throw new Error("编辑器尚未就绪，未执行文本恢复");
          // Opening the real native modified side also retains its model when
          // this file was previously closed, so the operation has a real undo stack.
          const documentPath = absoluteWorkspacePath(cwd, path);
          await editor.openDiff(documentPath, diff.original, `${path} · 文本恢复`);
          const snapshot = await editor.getDocument(documentPath, true);
          if (!isTaskCurrent()) return;
          if (!snapshot.exists || snapshot.source !== "buffer" || snapshot.version === null)
            throw new Error("原生编辑文档尚未载入，未执行文本恢复");
          if (snapshot.truncated || snapshot.content !== diff.modified)
            throw new Error("编辑器内容在确认后发生变化，未覆盖当前内容，请刷新后重试");
          mutation = await prepareManualMutation();
          if (!mutation || !isTaskCurrent()) return;
          const lines = snapshot.content.split("\n");
          assertCodingWorkspaceCancellationClear(cwd);
          const result = await editor.applyEdits({
            operationId: crypto.randomUUID(), sessionId: task.sessionId ?? task.id,
            runId: `manual:${crypto.randomUUID()}`, workspace: snapshot.workspace,
            files: [{ uri: snapshot.uri, expectedVersion: snapshot.version, expectedHash: snapshot.hash, save: true,
              edits: [{range:{start:{line:0,character:0},end:{line:lines.length-1,character:lines[lines.length-1].length}},text:diff.original}] }],
          });
          if (result.status !== "applied" || result.files.some(file => file.status !== "applied" || file.saveError))
            throw new Error(JSON.stringify(result));
          await finishManualMutation(mutation);
          if (isTaskCurrent()) setTheiaPanel("agent");
          onToast?.(`已恢复 ${path} 的文本，可在编辑器中撤销`);
        } else {
          mutation = await prepareManualMutation();
          if (!mutation || !isTaskCurrent()) return;
          await withWorkspaceDiskMutation(cwd, () => {
            assertCodingWorkspaceCancellationClear(cwd);
            return codingApi.discardFile(cwd, task.id, path);
          });
          await finishManualMutation(mutation);
          onToast?.(`已通过文件操作恢复 ${path}，此操作未加入文本撤销栈`);
        }
      } catch (error) {
        const message = String(error).replace(/^Error:\s*/, "");
        if (mutation) await blockInterruptedManualMutation(
          mutation,
          `丢弃文件改动失败：${message}`,
        );
        onToast?.(`丢弃失败：${message}`);
      } finally {
        setBusyPath(null);
      }
    },
    [
      blockInterruptedManualMutation,
      confirmTaskAction,
      cwd,
      finishManualMutation,
      onToast,
      prepareManualMutation,
      prepareEditor,
      isTaskCurrent,
      theiaFrameRef,
      task,
    ],
  );

  const commitChanges = useCallback(
    async (paths: string[], hunks: Record<string, string[]>) => {
      if (!cwd || !task || codingWorkspaceCancellationBlocked(cwd)) return;
      try {
        await prepareEditor();
        if (!isTaskCurrent()) return;
        const input = await codingApi.commitInput(cwd, task.id);
        requestTaskInput({
          title: "提交任务变更",
          description: `将创建选中 ${paths.length + Object.keys(hunks).length} 个文件或差异块的本地提交。检查证据将按实际候选内容核对；未选变更保留，可在同一会话继续提交。`,
          confirmLabel: "创建提交",
          fields: [
            {
              name: "message",
              label: "提交信息",
              defaultValue:
                input.split("\n")[0]?.replace(/^任务名称：/, "") ?? "",
              required: true,
              maxLength: 200,
            },
          ],
          action: async (values) => {
            assertCodingWorkspaceCancellationClear(cwd);
            setCommitting(true);
            try {
              const hash = await codingApi.commit(
                cwd,
                task.id,
                values.message.trim(),
                paths,
                hunks,
              );
              await useTaskStore.getState().refreshTaskState();
              onToast?.(`已提交 ${hash.slice(0, 8)}`);
            } finally {
              setCommitting(false);
            }
          },
          onError: (error) => {
            void useTaskStore.getState().refreshTaskState();
            onToast?.(`提交失败：${String(error).replace(/^Error:\s*/, "")}`);
          },
        });
      } catch (error) {
        onToast?.(`提交失败：${String(error).replace(/^Error:\s*/, "")}`);
      }
    },
    [cwd, onToast, requestTaskInput, task, prepareEditor, isTaskCurrent],
  );

  const openChangeDiff = useCallback(
    async (path: string) => {
      if (!task) return;
      try {
        const diff = await codingApi.changeDiff(cwd, task.id, path);
        if (!isTaskCurrent()) return;
        if (diff.binary) {
          onToast?.("此文件无法显示文本差异，请在专用工具中核对内容与元数据");
          return;
        }
        const editor = theiaFrameRef.current;
        if (!editor) throw new Error("编辑器尚未就绪");
        await editor.openDiff(absoluteWorkspacePath(cwd, path), diff.original, `${path} · 本轮修改`);
        if (isTaskCurrent()) setTheiaPanel("agent");
      } catch (error) {
        onToast?.(`打开差异失败：${String(error).replace(/^Error:\s*/, "")}`);
      }
    },
    [cwd, task, isTaskCurrent, onToast, theiaFrameRef],
  );

  const discardHunk = useCallback(
    async (path: string, hunkId: string) => {
      if (!task || streaming || runningVerification || sending || stopping || codingWorkspaceCancellationBlocked(cwd))
        return;
      if (changeSet?.changes.find(change => change.path === path)?.kind === "deleted") {
        onToast?.("已删除文件请按文件恢复，以保留原有权限和可执行位；仍可选择差异块提交");
        return;
      }
      const editor = theiaFrameRef.current;
      if (!editor) {
        onToast?.("编辑器尚未就绪");
        return;
      }
      setBusyPath(path);
      let mutation: TheiaMutationTicket | null = null;
      try {
        const proposal = await codingApi.revertHunkPreview(
          cwd,
          task.id,
          path,
          hunkId,
        );
        if (!isTaskCurrent()) return;
        const documentPath = absoluteWorkspacePath(cwd, path);
        // Load the actual modified file, including a previously closed file,
        // before applying an edit that promises native editor undo.
        await editor.openDiff(documentPath, proposal.modified, `${path} · 差异块恢复`);
        const snapshot = await editor.getDocument(documentPath, true);
        if (!isTaskCurrent()) return;
        if (!snapshot.exists || snapshot.source !== "buffer" || snapshot.version === null)
          throw new Error("原生编辑文档尚未载入，请按文件恢复已删除文件");
        if (snapshot.truncated || snapshot.content !== proposal.original)
          throw new Error("编辑器内容已变化，请保存或处理冲突后重新选择差异块");
        mutation = await prepareManualMutation();
        if (!mutation || !isTaskCurrent()) return;
        const lines = snapshot.content.split("\n");
        assertCodingWorkspaceCancellationClear(cwd);
        const result = await editor.applyEdits({
          operationId: crypto.randomUUID(),
          sessionId: task.sessionId ?? task.id,
          runId: `manual:${crypto.randomUUID()}`,
          workspace: snapshot.workspace,
          files: [
            {
              uri: snapshot.uri,
              expectedVersion: snapshot.version,
              expectedHash: snapshot.hash,
              save: true,
              edits: [
                {
                  range: {
                    start: { line: 0, character: 0 },
                    end: {
                      line: lines.length - 1,
                      character: lines[lines.length - 1].length,
                    },
                  },
                  text: proposal.modified,
                },
              ],
            },
          ],
        });
        if (
          result.status !== "applied" ||
          result.files.some((file) => file.status !== "applied" || file.saveError)
        )
          throw new Error(JSON.stringify(result));
        await finishManualMutation(mutation);
        if (isTaskCurrent()) setTheiaPanel("agent");
        setReportRevision((value) => value + 1);
        onToast?.("已撤销所选差异块，可在编辑器中撤销此操作");
      } catch (error) {
        if (mutation)
          await blockInterruptedManualMutation(
            mutation,
            `撤销差异块失败：${String(error)}`,
          );
        onToast?.(`撤销差异块失败：${String(error).replace(/^Error:\s*/, "")}`);
      } finally {
        setBusyPath(null);
      }
    },
    [
      task,
      changeSet,
      streaming,
      runningVerification,
      sending,
      stopping,
      cwd,
      isTaskCurrent,
      prepareManualMutation,
      finishManualMutation,
      blockInterruptedManualMutation,
      onToast,
      theiaFrameRef,
    ],
  );

  const runRedTest = useCallback(
    (command: DetectedCommand) => {
      if (!cwd || !task || command.kind !== "test" || redRunning || codingWorkspaceCancellationBlocked(cwd)) return;
      const taskId = task.id;
      const execute = async () => {
        setRedRunning(true);
        const runId = crypto.randomUUID();
        setRedRunId(runId);
        try {
          await prepareEditor();
          if (!isWorkspaceCurrent()) return;
          const token = command.requiresApproval
            ? await codingApi.approvePlanCommand(cwd, taskId, command.command)
            : undefined;
          assertCodingWorkspaceCancellationClear(cwd);
          const record = await codingApi.runVerification(
            cwd,
            taskId,
            "test",
            command.command,
            undefined,
            runId,
            token,
          );
          if (record.status !== "failed") {
            onToast?.(
              record.status === "passed"
                ? "测试已通过，尚未形成 RED 证据；请检查断言是否覆盖预期行为"
                : "红灯测试未得到有效失败结果，请查看命令输出后重试",
            );
            return;
          }
          setTddEvidence(await codingApi.recordRedTest(cwd, taskId, record.id));
          onToast?.("RED 失败已记录；现在可以继续实现并运行 GREEN 验证");
        } catch (error) {
          onToast?.(`红灯验证失败：${String(error).replace(/^Error:\s*/, "")}`);
        } finally {
          await useTaskStore
            .getState()
            .refreshTaskState()
            .catch(() => undefined);
          setRedRunId(null);
          setRedRunning(false);
        }
      };
      if (command.requiresApproval) {
        requestTaskConfirmation({
          title: "确认运行红灯测试",
          description: `以下计划命令将在当前任务工作目录运行：${command.command}`,
          confirmLabel: "确认并运行",
          danger: true,
          action: execute,
        });
      } else {
        void execute();
      }
    },
    [
      cwd,
      isWorkspaceCurrent,
      onToast,
      prepareEditor,
      redRunning,
      requestTaskConfirmation,
      task,
    ],
  );

  const integrateTask = useCallback(async () => {
    if (!cwd || !task || !isolatedWorkspace || integrating || codingWorkspaceCancellationBlocked(cwd) || codingWorkspaceCancellationBlocked(isolatedWorkspace.sourceRoot)) return;
    const confirmed = await confirmTaskAction({
      title: "应用到原项目",
      description: `将隔离任务的已提交变更应用到 ${isolatedWorkspace.sourceRoot}。原项目必须没有未提交改动，也不能有正在执行的任务；如发生冲突，原项目不会被覆盖。`,
      confirmLabel: "应用变更",
    });
    if (!confirmed) return;
    setIntegrating(true);
    try {
      assertCodingWorkspaceCancellationClear(cwd);
      assertCodingWorkspaceCancellationClear(isolatedWorkspace.sourceRoot);
      const updated = await codingApi.integrateIsolatedTask(cwd, task.id);
      setIsolatedWorkspace(updated);
      onToast?.("修改已应用到原项目");
    } catch (error) {
      onToast?.(`应用失败：${String(error).replace(/^Error:\s*/, "")}`);
    } finally {
      setIntegrating(false);
    }
  }, [confirmTaskAction, cwd, integrating, isolatedWorkspace, onToast, task]);

  const changeTaskModel = useCallback(
    async (nextModelId: string) => {
      const previous = modelId;
      setModelId(nextModelId);
      // The starter's choice belongs to the next coding session. Do not mutate
      // an unrelated chat session merely because it was active before Echo Code
      // opened; `startTask` passes this explicit model to the new session.
      if (!task) return;
      try {
        if (!task.sessionId || hostSessionId !== task.sessionId) {
          throw new Error("任务会话尚未完成恢复");
        }
        await onChangeModel?.(nextModelId);
        if (cwd) {
          await codingApi.bindTaskRuntime(
            cwd,
            task.id,
            task.sessionId,
            nextModelId,
          );
          await useTaskStore.getState().refreshTaskState();
        }
      } catch (error) {
        setModelId(previous);
        onToast?.(`切换模型失败：${String(error).replace(/^Error:\s*/, "")}`);
      }
    },
    [cwd, hostSessionId, modelId, onChangeModel, onToast, task],
  );

  const taskChangeCount = changeSet?.changes.length ?? 0;
  const theiaDisplayPanel = task ? theiaPanel : "agent";
  const theiaPhase = task ? describeTaskProgress(task.phase) : null;

  const beforeTheiaMutation = useCallback(
    async (operation: string, _paths: string[]) => {
      assertCodingWorkspaceCancellationClear(cwd);
      if (operation === "createFolder") {
        // An empty directory has no task diff. Gate verification, but defer a
        // follow-up round until a file inside it actually changes.
        const phase = useTaskStore.getState().task?.phase;
        if (
          phase &&
          ![
            "discovering",
            "implementing",
            "repairing",
            "paused",
            "stopped",
            "blocked",
            "delivered",
          ].includes(phase)
        ) {
          onToast?.(
            phase === "verifying"
              ? "正在验证当前改动，请等待验证结束后再新建文件夹"
              : "当前任务阶段不能新建文件夹",
          );
          return null;
        }
        return { taskId: null, closeRound: false };
      }
      return prepareManualMutation();
    },
    [cwd, onToast, prepareManualMutation],
  );
  const afterTheiaMutation = useCallback(
    async (ticket: TheiaMutationTicket, success: boolean, error?: string) => {
      if (success) {
        await finishManualMutation(ticket);
      } else {
        await blockInterruptedManualMutation(
          ticket,
          error ?? "Theia 文件操作失败",
        );
      }
    },
    [blockInterruptedManualMutation, finishManualMutation],
  );

  if (!cwd) {
    return (
      <div
        ref={workbenchRef}
        className="coding-workbench coding-workbench--empty"
      >
        <header className="coding-workbench__topbar" data-tauri-drag-region>
          <WorkbenchIdentity onExit={exitSafely} />
        </header>
        <div className="coding-workbench__welcome">
          <FolderGit2 size={32} />
          <h1>打开代码库开始开发</h1>
          <p>在同一个工作台里理解代码、交给 Agent 实现、审阅变更并交付。</p>
          <button
            type="button"
            className="coding-primary-btn"
            onClick={() => void pickWorkspace()}
          >
            <FolderGit2 size={15} /> 选择代码文件夹
          </button>
          {workspaces.length > 0 && (
            <ul className="coding-workbench__recent">
              {workspaces.slice(0, 5).map((workspace) => (
                <li key={workspace.cwd}>
                  <button
                    type="button"
                    onClick={() => onSelectWorkspace?.(workspace.cwd)}
                  >
                    {basename(workspace.cwd)}
                    <small>{idePath(workspace.cwd)}</small>
                  </button>
                </li>
              ))}
            </ul>
          )}
        </div>
      </div>
    );
  }

  return (
    <div
      ref={workbenchRef}
      className={`coding-workbench coding-workbench--theia${theiaAgentOpen ? "" : " coding-workbench--agent-closed"}`}
      style={{ "--echo-agent-width": `${agentWidth}px` } as CSSProperties}
    >
      <header className="coding-workbench__topbar" data-tauri-drag-region>
        <WorkbenchIdentity onExit={exitSafely}>
          <ProjectSwitcher
            projects={recentCodingProjects.map((project) => ({
              ...project,
              label: projectLabels[project.cwd],
            }))}
            activeCwd={activeCodingWorkspaceCwd || cwd}
            dirtyCount={visibleDirtyCount}
            onSelect={(nextCwd) => void switchProject(nextCwd)}
            onRemove={removeRecentProject}
            onOpenFolder={openAnotherProject}
            onManageWorktrees={() => setWorktreesOpen(true)}
          />
        </WorkbenchIdentity>
        <div className="coding-workbench__topbar-right" data-tauri-drag-region>
          <div className="echo-theia-preview" ref={theiaPreviewRef}>
            <button
              type="button"
              className="echo-theia-toolbar-button"
              aria-label="网页预览"
              aria-expanded={theiaPreviewOpen}
              onClick={() => setTheiaPreviewOpen((open) => !open)}
            >
              <PanelTop size={15} /> <span>网页预览</span>
            </button>
            {theiaPreviewOpen && (
              <form
                className="echo-theia-preview-form"
                onKeyDown={(event) => {
                  if (event.key === "Escape") setTheiaPreviewOpen(false);
                }}
                onSubmit={(event) => {
                  event.preventDefault();
                  const raw = theiaPreviewInput.trim();
                  if (!raw) return;
                  try {
                    const url = new URL(
                      raw.startsWith("http://") || raw.startsWith("https://")
                        ? raw
                        : `http://${raw}`,
                    );
                    setTheiaPreviewRequest({
                      url: url.toString(),
                      id: Date.now(),
                    });
                    setTheiaPreviewOpen(false);
                  } catch {
                    onToast?.("请输入有效的预览地址，例如 localhost:5173");
                  }
                }}
              >
                {previewUrls.length > 0 && (
                  <div className="echo-theia-preview-form__detected">
                    <span>输出中发现的本机地址</span>
                    {previewUrls.map((url) => (
                      <button
                        key={url}
                        type="button"
                        onClick={() => {
                          setTheiaPreviewRequest({ url, id: Date.now() });
                          setTheiaPreviewOpen(false);
                        }}
                      >
                        {url}
                      </button>
                    ))}
                  </div>
                )}
                <label htmlFor="echo-theia-preview-url">预览地址</label>
                <div>
                  <input
                    id="echo-theia-preview-url"
                    aria-label="网页预览地址"
                    autoFocus
                    placeholder="localhost:5173"
                    value={theiaPreviewInput}
                    onChange={(event) =>
                      setTheiaPreviewInput(event.target.value)
                    }
                  />
                  <button type="submit">打开</button>
                </div>
              </form>
            )}
          </div>
          <button
            type="button"
            className="coding-icon-btn"
            onClick={() => setTheiaAgentOpen((open) => !open)}
            aria-label={theiaAgentOpen ? "收起 Agent 面板" : "展开 Agent 面板"}
            aria-pressed={theiaAgentOpen}
          >
            {theiaAgentOpen ? (
              <PanelRightClose size={16} />
            ) : (
              <PanelRightOpen size={16} />
            )}
          </button>
          <ActionMenu
            label="编辑器选项"
            menuLabel="编辑器选项"
            triggerClassName="coding-icon-btn"
            menuClassName="topbar-actions__menu coding-editor-options"
            resetKey={cwd}
            trigger={<Settings2 size={15} />}
          >
            {({ close }) => <>
              <button
                type="button"
                className="topbar-actions__item"
                role="menuitemradio"
                aria-checked={completionEnabled}
                onClick={() => {
                  setCompletionEnabled((value) => !value);
                  close(true);
                }}
              >
                <span className="coding-editor-options__check">{completionEnabled && <Check size={14} />}</span>
                <span className="coding-editor-options__copy">
                  <span>Tab 代码补全</span>
                  <small>{completionEnabled ? completionStatus : "已关闭"}</small>
                </span>
              </button>
              <div className="coding-project-switcher__separator" role="separator" />
              <button type="button" className="topbar-actions__item" role="menuitem" onClick={() => { close(); onOpenSettings?.(); }}>
                <Settings2 size={14} />
                <span>模型与 Agent 设置…</span>
              </button>
            </>}
          </ActionMenu>
        </div>
      </header>

      <main className="echo-theia-workspace">
        <TheiaIdeFrame
          ref={theiaFrameRef}
          root={cwd}
          modelId={modelId}
          completionEnabled={completionEnabled}
          onCompletionStatus={setCompletionStatus}
          onBeforeMutation={beforeTheiaMutation}
          onAfterMutation={afterTheiaMutation}
          onActiveFile={setTheiaActiveFile}
          onActiveSymbol={setTheiaActiveSymbol}
          onPreviewUrl={onDetectedPreviewUrl}
          onToast={onToast}
          previewRequest={theiaPreviewRequest}
          openFileRequest={theiaOpenFileRequest}
          onDirtyChange={(count) => {
            if (count !== theiaDirtyCount) {
              setDeliveryReport(null);
              setReportRevision((value) => value + 1);
            }
            setTheiaDirtyCount(count);
          }}
        />
        {theiaReportOpen && task && (
          <section
            className="echo-theia-review echo-theia-report"
            aria-label="交付报告"
          >
            <header className="echo-theia-review__header">
              <strong>交付报告</strong>
              <button
                type="button"
                aria-label="关闭交付报告"
                onClick={() => setTheiaReportOpen(false)}
              >
                <X size={16} />
              </button>
            </header>
            <DeliveryReportTab
              root={cwd}
              taskId={task.id}
              revision={reportRevision}
              onOpenFile={(path) => {
                openTheiaFile(path);
                setTheiaReportOpen(false);
              }}
              onToast={onToast}
              onReviewChanged={() => setReportRevision((value) => value + 1)}
              onTddChanged={setTddEvidence}
            />
          </section>
        )}
        {theiaPlanOpen && task && (
          <section
            className="echo-theia-review echo-theia-plan"
            aria-label="执行计划"
          >
            <header className="echo-theia-review__header">
              <strong>执行计划</strong>
              <button
                type="button"
                aria-label="关闭执行计划"
                onClick={() => setTheiaPlanOpen(false)}
              >
                <X size={16} />
              </button>
            </header>
            <TaskDagTab
              task={task}
              repairRounds={orchestrator?.repairRounds ?? []}
              maxRepairRounds={orchestrator?.maxRepairRounds ?? 3}
              changedFileCount={changeSet?.changes.length ?? 0}
              problemCount={problems.length}
              ledger={ledger}
              onOpenFile={(path) => {
                openTheiaFile(path);
                setTheiaPlanOpen(false);
              }}
            />
          </section>
        )}
        {analysisSymbol && (
          <section
            className="echo-theia-review echo-theia-analysis"
            aria-label="代码影响分析"
          >
            <header className="echo-theia-review__header">
              <div>
                <GitBranch size={15} />
                <strong>{analysisSymbol}</strong>
                <span>近似代码分析</span>
              </div>
              <button
                type="button"
                aria-label="关闭代码影响分析"
                onClick={() => setAnalysisSymbol(null)}
              >
                <X size={16} />
              </button>
            </header>
            <div
              className="echo-theia-analysis__tabs"
              role="tablist"
              aria-label="代码分析视图"
            >
              <button
                type="button"
                role="tab"
                aria-selected={analysisMode === "impact"}
                onClick={() => setAnalysisMode("impact")}
              >
                潜在影响
              </button>
              <button
                type="button"
                role="tab"
                aria-selected={analysisMode === "references"}
                onClick={() => setAnalysisMode("references")}
              >
                引用线索
              </button>
            </div>
            <p className="echo-theia-analysis__notice">
              依据文本匹配和符号索引推测，结果可能有误报或漏报。修改前请结合语言服务与测试核对。
            </p>
            <div className="echo-theia-analysis__body">
              {analysisError ? (
                <div className="coding-views__error">
                  索引失败：{analysisError}{" "}
                  <button
                    type="button"
                    onClick={() => setAnalysisAttempt((value) => value + 1)}
                  >
                    重试
                  </button>
                </div>
              ) : !analysisReady ? (
                <div className="coding-views__empty">
                  <LoaderCircle size={15} className="is-spinning" />{" "}
                  正在建立代码索引…
                </div>
              ) : analysisMode === "impact" ? (
                <ImpactAnalysisView
                  root={cwd}
                  symbol={analysisSymbol}
                  onOpenSymbol={(symbol) =>
                    openTheiaFile(symbol.path, symbol.line)
                  }
                />
              ) : (
                <FindReferencesView
                  root={cwd}
                  symbol={analysisSymbol}
                  onOpenSymbol={(symbol) =>
                    openTheiaFile(symbol.path, symbol.line)
                  }
                />
              )}
            </div>
          </section>
        )}
        {task && theiaDisplayPanel !== "agent" && (
          <section
            className="echo-theia-review coding-workspace-detail"
            aria-label={
              theiaDisplayPanel === "changes" ? "工程变更" : "工程检查"
            }
          >
            <header className="echo-theia-review__header">
              <strong>
                {theiaDisplayPanel === "changes" ? "变更与提交" : "检查与输出"}
              </strong>
              <button
                type="button"
                aria-label="返回编辑器"
                onClick={() => setTheiaPanel("agent")}
              >
                <X size={16} />
              </button>
            </header>
            {theiaDisplayPanel === "changes" && (
              <ChangeSetView
                changeSet={changeSet}
                hasTask={Boolean(task)}
                busyPath={busyPath}
                committing={committing}
                integrating={integrating}
                isolatedSource={isolatedWorkspace?.sourceRoot}
                integratedHash={isolatedWorkspace?.integratedHash}
                canCommit={
                  !streaming && !runningVerification && !sending && !stopping && !documentCancellationBlocked
                }
                canRollback={
                  !documentCancellationBlocked &&
                  !runningVerification &&
                  !streaming &&
                  Boolean(
                    task &&
                      ["paused", "stopped", "delivered", "blocked"].includes(
                        task.phase,
                      ),
                  )
                }
                canDiscard={
                  !documentCancellationBlocked &&
                  !runningVerification &&
                  !streaming &&
                  !sending &&
                  Boolean(
                    task &&
                      [
                        "implementing",
                        "repairing",
                        "discovering",
                        "paused",
                        "stopped",
                        "blocked",
                        "delivered",
                      ].includes(task.phase),
                  )
                }
                onOpenDiff={(change) => {
                  setTheiaReportOpen(false);
                  void openChangeDiff(change.path);
                }}
                onDiscard={(change) => void discardChange(change.path)}
                onCommit={(paths, hunks) => void commitChanges(paths, hunks)}
                onLoadHunks={(path) =>
                  codingApi.commitHunks(cwd, task!.id, path)
                }
                onDiscardHunk={(path, hunkId) => void discardHunk(path, hunkId)}
                onRollback={() => void rollbackTask()}
                onIntegrate={() => void integrateTask()}
              />
            )}
            {theiaDisplayPanel === "changes" && task && (
              <RunHistory
                root={cwd}
                taskId={task.id}
                revision={`${task.updatedAt}:${reportRevision}`}
                currentRevision={freshness?.contentRevision}
                dirty={visibleDirtyCount > 0}
                busy={streaming || runningVerification || stopping || documentCancellationBlocked}
                onOpenFile={openTheiaFile}
                onFixReview={async (review) => {
                  setIntent("agent");
                  const accepted = await sendFollowup(
                    `请核对以下审查问题与当前代码，修复仍然存在的问题并运行相关检查。审查基于 revision=${review.contentRevision}，若代码已变化请先重新核对，不机械套用旧建议。\n${review.summary}`,
                    true,
                    undefined,
                    [],
                    "agent",
                  );
                  if (accepted) setTheiaPanel("agent");
                }}
                onChanged={async () => {
                  await useTaskStore.getState().refreshTaskState();
                  setReportRevision(value => value + 1);
                }}
              />
            )}
            {theiaDisplayPanel === "verification" && (
              <>
                <fieldset disabled={documentCancellationBlocked} style={{border:0,padding:0,margin:0}}><ProjectChecksEditor
                  root={cwd}
                  onSaved={() => {
                    void codingApi.detectCommands(cwd).then(setDetected);
                    setReportRevision((value) => value + 1);
                  }}
                /></fieldset>
                <VerificationView
                  records={verifications}
                  currentRevision={freshness?.contentRevision}
                  currentConfigurationRevision={
                    freshness?.configurationRevision
                  }
                  dirty={visibleDirtyCount > 0}
                  detected={verificationCommands}
                  running={runningVerification || redRunning}
                  hasTask={Boolean(task)}
                  executionBlocked={documentCancellationBlocked}
                  redCheckpoint={
                    task?.phase === "paused" &&
                    task.phaseReason === "等待红灯测试验证"
                  }
                  redRecorded={Boolean(
                    tddEvidence?.redRecordId || tddEvidence?.waiverReason,
                  )}
                  onRunRed={runRedTest}
                  onRun={(command) => void runVerifications([command])}
                  onRunAll={() => void runVerifications(verificationCommands)}
                  onCancel={() => {
                    if (activeRunId || redRunId || backgroundRunId)
                      void codingApi.cancelVerification(
                        (activeRunId || redRunId || backgroundRunId)!,
                      );
                  }}
                  onOpenOutput={(record) => {
                    setSelectedVerification(record);
                    setTheiaVerificationOutput(
                      [record.stdout, record.stderr].filter(Boolean).join("\n"),
                    );
                  }}
                />
                {(runningVerification ||
                  theiaVerificationOutput ||
                  commandOutput) && (
                  <div
                    className="echo-theia-agent__output-panel"
                    aria-label="验证命令输出"
                  >
                    <strong>
                      {runningVerification
                        ? "实时输出"
                        : theiaVerificationOutput
                          ? "历史验证输出"
                          : "最近一次验证输出"}
                    </strong>
                    {selectedVerification && !runningVerification && (
                      <div className="coding-panel__actions">
                        <button
                          type="button"
                          onClick={() => {
                            const text = JSON.stringify({
                              ...selectedVerification,
                              workspace: cwd,
                              stdout: selectedVerification.stdout.slice(
                                0,
                                8000,
                              ),
                              stderr: selectedVerification.stderr.slice(
                                0,
                                8000,
                              ),
                              truncated:
                                selectedVerification.stdout.length > 8000 ||
                                selectedVerification.stderr.length > 8000,
                            });
                            setExtraContext((current) => [
                              ...current.filter(
                                (item) =>
                                  item.id !==
                                  `check:${selectedVerification.id}`,
                              ),
                              {
                                id: `check:${selectedVerification.id}`,
                                label: `检查 ${selectedVerification.command}`,
                                text,
                              },
                            ]);
                            onToast?.("已添加该次检查的命令、目录、版本与输出");
                          }}
                        >
                          加入下一轮上下文
                        </button>
                        <button
                          type="button"
                          disabled={streaming || sending || stopping}
                          onClick={() => {
                            const evidence = JSON.stringify({
                              ...selectedVerification,
                              workspace: cwd,
                              stdout: selectedVerification.stdout.slice(
                                0,
                                8000,
                              ),
                              stderr: selectedVerification.stderr.slice(
                                0,
                                8000,
                              ),
                              truncated:
                                selectedVerification.stdout.length > 8000 ||
                                selectedVerification.stderr.length > 8000,
                            });
                            void sendFollowup(
                              `请根据以下真实检查记录核对当前代码并修复问题；版本或环境已变化时先重新确认，不把旧输出当成当前结论。\n${evidence}`,
                              true,
                              undefined,
                              [],
                              "agent",
                            )
                              .then((accepted) => {
                                if (accepted) {
                                  chooseIntent("agent");
                                  setTheiaPanel("agent");
                                }
                              })
                              .catch((error) => onToast?.(String(error)));
                          }}
                        >
                          交给 Agent 修复
                        </button>
                      </div>
                    )}
                    <pre className="echo-theia-agent__output">
                      {runningVerification
                        ? commandOutput || "正在等待命令输出…"
                        : theiaVerificationOutput || commandOutput}
                    </pre>
                  </div>
                )}
              </>
            )}
          </section>
        )}
        {worktreesOpen && (
          <section
            className="echo-theia-review coding-workspace-detail"
            aria-label="受管理的工作树"
          >
            <header className="echo-theia-review__header">
              <strong>隔离工作树</strong>
              <button
                type="button"
                onClick={() => setWorktreesOpen(false)}
                aria-label="关闭工作树管理"
              >
                <X size={16} />
              </button>
            </header>
            <ManagedWorktrees
              root={cwd}
              onOpen={(root) => void switchProject(root)}
              onRemoved={(root) => onCloseCodingWorkspace?.(root)}
              confirm={confirmTaskAction}
            />
          </section>
        )}
        {contextPreview && (
          <section
            className="echo-theia-review coding-workspace-detail"
            aria-label="上下文预览"
          >
            <header className="echo-theia-review__header">
              <strong>本轮上下文预览 · 来源与版本</strong>
              <button
                type="button"
                onClick={() => setContextPreview(null)}
                aria-label="关闭上下文预览"
              >
                <X size={16} />
              </button>
            </header>
            <pre className="coding-context-preview">{contextPreview}</pre>
          </section>
        )}
        {diagnosticPicker && (
          <section
            className="echo-theia-review coding-workspace-detail"
            aria-label="选择诊断上下文"
          >
            <header className="echo-theia-review__header">
              <strong>选择 Problems · {diagnosticPicker.length} 条</strong>
              <button
                type="button"
                onClick={() => setDiagnosticPicker(null)}
                aria-label="关闭诊断选择"
              >
                <X size={16} />
              </button>
            </header>
            <div className="coding-diagnostic-picker">
              {diagnosticPicker.map((diagnostic, index) => {
                const id = `diagnostic:${diagnostic.uri}:${diagnostic.range.start.line}:${diagnostic.range.start.character}:${index}`;
                return (
                  <label key={id}>
                    <input
                      type="checkbox"
                      checked={extraContext.some((item) => item.id === id)}
                      onChange={(event) => {
                        const checked = event.target.checked;
                        setExtraContext((current) =>
                          checked
                            ? [
                                ...current.filter((item) => item.id !== id),
                                {
                                  id,
                                  label: `${diagnostic.severity} ${diagnostic.range.start.line + 1}:${diagnostic.range.start.character + 1}`,
                                  text: JSON.stringify({
                                    ...diagnostic,
                                    selected: true,
                                  }),
                                },
                              ]
                            : current.filter((item) => item.id !== id),
                        );
                      }}
                    />
                    <span>
                      {diagnostic.message}
                      <small>
                        {diagnostic.uri} · {diagnostic.range.start.line + 1}:
                        {diagnostic.range.start.character + 1} ·{" "}
                        {diagnostic.source} · version{" "}
                        {diagnostic.version ?? "unknown"}
                      </small>
                    </span>
                  </label>
                );
              })}
              {editorContext?.diagnosticsTruncated && (
                <p>诊断列表已截断，请在 Problems 中缩小范围后重新捕获。</p>
              )}
            </div>
          </section>
        )}
      </main>
      {theiaAgentOpen && (
        <div
          className="echo-theia-agent__splitter"
          role="separator"
          tabIndex={0}
          aria-label="调整 Agent 面板宽度"
          aria-orientation="vertical"
          aria-valuemin={300}
          aria-valuemax={agentMaximumWidth}
          aria-valuenow={agentWidth}
          title="拖动调整宽度，双击重置"
          onDoubleClick={resetAgentWidth}
          onKeyDown={(event) => {
            if (event.key === "ArrowLeft" || event.key === "ArrowRight") {
              event.preventDefault();
              resizeAgentByKey(event.key);
            }
          }}
          onPointerDown={(event) => {
            event.preventDefault();
            startAgentResize();
          }}
        />
      )}
      <aside
        className="echo-theia-agent"
        aria-label="Coding Agent"
        aria-hidden={!theiaAgentOpen}
      >
        <div className="echo-theia-agent__heading">
          <TaskSwitcher
            tasks={summaries}
            activeId={task?.id}
            activeName={task?.name}
            newDisabled={creatingIsolatedWorkspace || documentCancellationBlocked}
            onSelect={(taskId) => void activateCodingTask(taskId)}
            onNew={beginNewTask}
            onRename={renameCodingTask}
            onDelete={deleteCodingTask}
          />
          {task && theiaPhase?.active && (
            <div className="echo-theia-agent__task-status">
              <button
                type="button"
                className="echo-theia-agent__stop"
                onClick={() => void stopActiveTask()}
                disabled={sending || lifecycleSettling || stopping || documentCancellationBlocked}
                title={stopping || documentCancellationPending ? "正在停止当前任务" : "停止当前任务"}
                aria-label="停止当前任务"
              >
                <Square size={12} />
              </button>
            </div>
          )}
        </div>
        {cancellationMessage ? (
          <small className="coding-panel__hint" role="status">{cancellationMessage}</small>
        ) : streaming && (
          <small className="coding-panel__hint" role="status">
            {stopping
              ? "正在停止，等待运行、编辑器或外部工具确认；已发生修改保留"
              : `当前 ${transcript?.agentMode === "ask" || transcript?.agentMode === "plan" ? transcript.agentMode : "agent"} · 下一轮 ${intent}`}
          </small>
        )}
        {task && (
          <div className="coding-session-actions" aria-label="会话操作">
            <button type="button" onClick={() => setTheiaPanel("changes")}>
              变更{taskChangeCount ? ` · ${taskChangeCount}` : ""}
            </button>
            <button type="button" onClick={() => setTheiaPanel("verification")}>
              检查
            </button>
            <details>
              <summary>更多</summary>
              <button
                type="button"
                onClick={() => setPlanVisible((open) => !open)}
              >
                会话计划
              </button>
              <button
                type="button"
                onClick={() => {
                  setTheiaReportOpen(true);
                }}
              >
                导出摘要
              </button>
              <button
                type="button"
                disabled={streaming || sending}
                onClick={() => void requestCodeReview()}
              >
                代码审查
              </button>
            </details>
          </div>
        )}
        {theiaActiveFile && (
          <div className="echo-theia-agent__context">
            <span title={theiaActiveFile}>
              {workspaceRelativePath(cwd, theiaActiveFile)}
            </span>
            <button
              type="button"
              onClick={() => addManyToContext([theiaActiveFile])}
            >
              加入上下文
            </button>
            {theiaActiveSymbol?.path === theiaActiveFile && (
              <button
                type="button"
                title={`查看 ${theiaActiveSymbol.symbol} 的潜在影响和引用线索`}
                onClick={() => {
                  setAnalysisMode("impact");
                  setAnalysisSymbol(theiaActiveSymbol.symbol);
                  setTheiaPlanOpen(false);
                  setTheiaReportOpen(false);
                }}
              >
                潜在影响
              </button>
            )}
          </div>
        )}
        <div className="echo-theia-agent__content">
          {task ? (
            <AgentPane
              deliveryReport={
                visibleDirtyCount > 0 || deliveryReport?.task?.id !== task.id
                  ? null
                  : deliveryReport
              }
              task={task}
              changeSet={changeSet}
              verifications={verifications}
              sessionId={activeSessionId}
              messages={messages}
              streaming={streaming}
              phaseReason={phaseReason ?? task.phaseReason ?? undefined}
              blocker={blocker !== undefined ? blocker : task.blocker}
              awaitingPermission={awaitingPermission}
              awaitingQuestion={awaitingQuestion}
              planApproval={
                activeSessionId &&
                Boolean(transcript?.planApprovals?.length) &&
                (hostSessionId === activeSessionId ? (
                  <PlanPanel sessionId={activeSessionId} onToast={onToast} />
                ) : (
                  <button
                    type="button"
                    className="btn btn--primary"
                    disabled={!onActivateSession}
                    onClick={() =>
                      void onActivateSession?.(activeSessionId, cwd).catch(
                        (error) =>
                          onToast?.(
                            `打开计划确认失败：${String(error).replace(/^Error:\s*/, "")}`,
                          ),
                      )
                    }
                  >
                    打开任务会话并确认计划
                  </button>
                ))
              }
              sending={sending || lifecycleSettling}
              executionPending={documentCancellationPending ? cancellationMessage : undefined}
              onContinue={continueInterruptedTask}
              continueDisabled={documentCancellationBlocked}
              onOpenVerification={() => setTheiaPanel("verification")}
              onOpenChanges={() => setTheiaPanel("changes")}
              onOpenReport={() => {
                setTheiaReportOpen(true);
              }}
              onOpenFile={openTheiaFile}
              onPathsDropped={(paths) => addManyToContext(paths)}
            />
          ) : (activeTaskCount > 0 || isolatedWorkspace) ? (
            <div className="echo-theia-agent__empty">
              {activeTaskCount > 0 && (
                <p>当前项目有任务正在执行；提交新任务时会创建隔离工作树。</p>
              )}
              {isolatedWorkspace && (
                <p>
                  隔离于 {isolatedWorkspace.sourceRoot} · 基于提交{" "}
                  {isolatedWorkspace.baseHead.slice(0, 8)}
                  ，不包含原项目未提交文件。
                </p>
              )}
            </div>
          ) : null}
        </div>
        {followupQueue.queueId && (
          <div className="echo-theia-agent__queue">
            <QueuePanel
              sessionId={followupQueue.queueId}
              awaitingQuestion={!followupQueue.manualReady}
              blockedReason={
                task?.phase === "verifying"
                  ? "正在验证，完成后可发送"
                  : "请先处理当前任务状态或待确认事项"
              }
              onSendNow={(_text, _paths, id) =>
                id ? followupQueue.sendItem(id) : false
              }
              hint="本轮结束后自动发送下一条；权限或问题阻塞时保留队列"
            />
          </div>
        )}
        {documentCancellationPending && <ExternalToolRecovery root={cwd} onRecovered={() => {
          void useTaskStore.getState().refreshTaskState();
        }} />}
        {planVisible && task && (
          <EditablePlan
            key={`${cwd}:${task.id}`}
            scope={`${cwd}:${task.id}`}
            initial={
              intent === "plan"
                ? (messages
                    .filter(
                      (message) =>
                        message.role === "assistant" && message.complete,
                    )
                    .slice(-1)[0]
                    ?.parts.filter((part) => part.kind === "text")
                    .map((part) => ("text" in part ? part.text : ""))
                    .join("\n") ?? "")
                : ""
            }
            busy={streaming || sending}
            onExecute={async (finalPlan) => {
              const accepted = await sendFollowup(
                `执行已编辑的最终计划：\n${finalPlan}`,
                true,
                undefined,
                [],
                "agent",
              );
              if (accepted) {
                chooseIntent("agent");
                setPlanVisible(false);
              }
              return accepted;
            }}
            onSave={(text) =>
              requestTaskInput({
                title: "保存计划到工程",
                description:
                  "仅写入指定的新 Markdown 文件，不授权其他工程修改。",
                confirmLabel: "保存计划",
                fields: [
                  {
                    name: "path",
                    label: "工程相对路径",
                    defaultValue: "coding-plan.md",
                    required: true,
                    maxLength: 200,
                  },
                ],
                action: async (values) => {
                  await codingApi.savePlan(cwd, values.path, text);
                  onToast?.("计划已保存，当前模式保持不变");
                },
              })
            }
          />
        )}
        <TheiaAgentComposer
          key={`${cwd}:${task?.id ?? "new"}`}
          workspaceRoot={cwd}
          task={task}
          sessionId={activeSessionId}
          models={models}
          modelId={modelId}
          intent={intent}
          onIntentChange={chooseIntent}
          includeEditor={includeEditor && !!(theiaActiveFile || editorContext?.document)}
          editorLabel={
            editorContext?.document?.selection &&
            editorContext.document.selectedText
              ? "当前选区"
              : "当前文件"
          }
          onRemoveEditor={() => setIncludeEditor(false)}
          onCaptureContext={(kind) =>
            void captureContext(kind).catch((error) => onToast?.(String(error)))
          }
          extraContext={extraContext}
          onRemoveExtraContext={(id) =>
            setExtraContext((current) =>
              current.filter((item) => item.id !== id),
            )
          }
          onPreviewContext={(id) =>
            setContextPreview(
              extraContext.find((item) => item.id === id)?.text ?? null,
            )
          }
          onCorrect={onCorrectRun}
          contextPaths={contextPaths}
          apiReady={apiReady}
          startError={startError}
          starting={starting || creatingIsolatedWorkspace}
          sending={sending || lifecycleSettling || followupQueue.sending}
          streaming={streaming}
          executionBlocked={documentCancellationBlocked}
          onModelChange={(next) => void changeTaskModel(next)}
          onStart={(text, attachments) =>
            startTask(text, undefined, [], attachments)
          }
          onSend={(text, attachments) =>
            sendFollowup(text, true, undefined, attachments)
          }
          onEnqueue={async (text, paths, images) =>
            followupQueue.enqueue(text, paths, {
              mode: intent,
              editorContext: await contextForTurn(),
              imagePaths: images,
            })
          }
          awaitingDecision={
            awaitingPermission ||
            awaitingQuestion ||
            Boolean(transcript?.planApprovals?.length)
          }
          onRemoveContext={removeFromContext}
          onDraftContextPaths={addManyToContext}
          onOpenSettings={onOpenSettings}
          onToast={onToast}
        />
      </aside>
      {taskDialog}
    </div>
  );
}
