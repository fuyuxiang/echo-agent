import { findModelDisplayName } from "@/lib/model-presentation";
import { PageToolbar } from "./PageToolbar";
/** Local scheduled-task workspace: plans, scoped run results and a draft-safe editor. */
import {
  useCallback,
  useEffect,
  useId,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import {
  AddIcon,
  AlarmClockIcon,
  ArchiveIcon,
  AtmAddFromTemplateIcon,
  AtmBatchManageIcon,
  AutomationEmptyAlarmIcon,
  AutomationEmptyRecordsIcon,
  CheckBoldIcon,
  CheckIcon,
  ChevronDownIcon,
  CirclePauseIcon,
  DeleteIcon,
  ErrorCircleIcon,
  PlayIcon,
  RunningStatusIcon,
  SearchIcon,
} from "@/foundation/components/Icon/icons";
import {
  agentsList,
  automationRecordsArchive,
  automationRecordsDelete,
  automationsDelete,
  automationsRun,
  automationsResetSession,
  automationRunStop,
  desktopPreferencesGet,
  automationsSave,
  automationsSetStatus,
  automationsSnapshot,
  agentAuthStatus,
  agentListWorkspaces,
  filterModelsByRuntimeCatalog,
  mcpList,
  providersList,
  flattenModels,
  skillsList,
  type WorkspaceInfo,
} from "@/lib/agent-client";
import type {
  AgentEntry,
  Automation,
  AutomationRunRecord,
  AutomationSnapshot,
  AutomationStatus,
  SkillInfo,
} from "@/lib/types";
import {
  AUTOMATION_TEMPLATES,
  type AutomationTemplate,
} from "./automation/template-config";
import {
  DAY_LABELS,
  automationFromDraft,
  buildDraft,
  draftFromAutomation,
  formatRunTime,
  scheduledAtIso,
  validateDraft,
  type AutomationDraft,
} from "./automation/schedule-utils";
import { AutomationTaskCard } from "./automation/AutomationTaskCard";
import { AutomationTaskDetail } from "./automation/AutomationTaskDetail";
import {
  localTimeZoneLabel,
  nextRunLabel,
  taskState,
  runStatusLabel,
  runBusy,
  type TaskFilter,
} from "./automation/task-presentation";
import { Segmented } from "./automation/controls";
import { AutomationTemplateGrid } from "./automation/AutomationTemplateGrid";
import {
  AutomationEditPage,
  type ModelOption,
} from "./automation/AutomationEditPage";
import { AutomationPermissionConfirmDialog } from "./automation/AutomationPermissionConfirmDialog";
import { usePermissionConfirm } from "./automation/usePermissionConfirm";
import type { ConnectorOption } from "./automation/ConnectorSelector";
import { useModalFocus } from "@/lib/use-modal-focus";
import { useUnsavedClose } from "@/lib/use-unsaved-close";
import { useAppDialog } from "./AppDialog";
import {
  useAutomationEditorStore,
  automationEditorDirty,
} from "@/stores/automation-editor-store";

interface AutomationPanelProps {
  onToast?: (msg: string) => void;
  onNavigate?: (label: string) => void;
  onOpenSession?: (sessionId: string, cwd?: string) => void;
  cwd?: string;
  /** Incremented when the native scheduler persists a lifecycle transition. */
  refreshSignal?: number;
  notificationAutomationId?: string;
  notificationAutomationSequence?: number;
  onOpenSettings?: (section: "general" | "notifications") => void;
}

type TabKey = "tasks" | "records";
type RecordFilter =
  | "all"
  | "success"
  | "failed"
  | "interrupted"
  | "running"
  | "archived"
  | "attention"
  | "stopped";
type ReferenceCatalog =
  | "workspaces"
  | "models"
  | "skills"
  | "experts"
  | "connectors";

const REFERENCE_CATALOG_LABELS: Record<ReferenceCatalog, string> = {
  workspaces: "工作空间",
  models: "模型",
  skills: "技能",
  experts: "专家",
  connectors: "连接器",
};

const RECORD_FILTERS: { key: RecordFilter; label: string }[] = [
  { key: "all", label: "全部" },
  { key: "success", label: "成功" },
  { key: "failed", label: "失败" },
  { key: "interrupted", label: "人工接管" },
  { key: "running", label: "进行中" },
  { key: "attention", label: "待确认" },
  { key: "stopped", label: "已停止" },
  { key: "archived", label: "已归档" },
];

function automationModelError(
  draft: AutomationDraft,
  models: ModelOption[],
  autoModelId?: string,
  runtimeError?: string,
  requireRunnable = draft.status === "ACTIVE",
): string | null {
  // Paused tasks remain editable while the Runtime is offline. The same task
  // is checked again before it is resumed or manually executed.
  if (!requireRunnable) return null;
  if (runtimeError) return runtimeError;
  if (!draft.modelId) {
    return autoModelId
      ? null
      : "当前没有可用的默认模型，请在「设置 → 模型」检查连接。";
  }
  return models.some((model) => model.id === draft.modelId)
    ? null
    : `已选模型“${findModelDisplayName(models, draft.modelId)}”当前不可用，请重新选择模型。`;
}

function RecordStatusIcon({ item }: { item: AutomationRunRecord }) {
  if (item.status === "queued") {
    return (
      <RunningStatusIcon
        size={16}
        color="var(--echo-color-text-disabled, #777)"
      />
    );
  }
  if (item.status === "running") {
    return (
      <span
        className="atm-status-icon-spinning"
        style={{ display: "inline-flex" }}
      >
        <RunningStatusIcon size={16} color="#00C29A" />
      </span>
    );
  }
  if (item.status === "success")
    return (
      <CheckBoldIcon size={16} color="var(--echo-color-text-disabled, #000)" />
    );
  if (item.status === "failed") return <ErrorCircleIcon size={16} />;
  if (item.status === "interrupted")
    return (
      <CirclePauseIcon
        width={16}
        height={16}
        color="var(--echo-color-text-secondary)"
      />
    );
  return <CheckIcon size={16} />;
}

/** 通用确认弹窗（替代 EchoAgent 的 Modal.confirm）。 */
function ConfirmDialog({
  title,
  content,
  okText,
  danger,
  onOk,
  onCancel,
}: {
  title: string;
  content: string;
  okText: string;
  danger?: boolean;
  onOk: () => void;
  onCancel: () => void;
}) {
  const titleId = useId();
  const contentId = useId();
  const dialogRef = useModalFocus<HTMLDivElement>(true, onCancel);

  return (
    <div className="modal-overlay" onClick={onCancel}>
      <div
        ref={dialogRef}
        className="atm-confirm-dialog"
        onClick={(e) => e.stopPropagation()}
        role="dialog"
        aria-modal="true"
        aria-labelledby={titleId}
        aria-describedby={contentId}
        tabIndex={-1}
      >
        <h3 id={titleId} className="atm-confirm-title">
          {title}
        </h3>
        <p id={contentId} className="atm-confirm-content">
          {content}
        </p>
        <div className="atm-confirm-actions">
          <button
            type="button"
            className="atm-btn atm-btn--secondary"
            onClick={onCancel}
            data-modal-initial-focus
          >
            取消
          </button>
          <button
            type="button"
            className={`atm-btn ${danger ? "atm-btn--danger" : "atm-btn--primary"}`}
            onClick={onOk}
          >
            {okText}
          </button>
        </div>
      </div>
    </div>
  );
}

export function AutomationPanel({
  onToast,
  onNavigate,
  onOpenSession,
  cwd,
  refreshSignal = 0,
  notificationAutomationId,
  notificationAutomationSequence,
  onOpenSettings,
}: AutomationPanelProps) {
  // ---------- 数据 ----------
  const [snapshot, setSnapshot] = useState<AutomationSnapshot | null>(null);
  const [loading, setLoading] = useState(true);
  const [snapshotError, setSnapshotError] = useState<string | null>(null);
  const [editorError, setEditorError] = useState<string | null>(null);
  const snapshotGenerationRef = useRef(0);
  const lastRefreshSignalRef = useRef(refreshSignal);
  const [runStartingIds, setRunStartingIds] = useState<Set<string>>(new Set());
  const [activeTab, setActiveTab] = useState<TabKey>(
    notificationAutomationId ? "records" : "tasks",
  );
  const [focusedAutomationId, setFocusedAutomationId] = useState<string | null>(
    notificationAutomationId ?? null,
  );
  const [searchQuery, setSearchQuery] = useState("");
  const [filterStatus, setFilterStatus] = useState<RecordFilter>("all");
  const [filterOpen, setFilterOpen] = useState(false);
  const filterRef = useRef<HTMLDivElement>(null);
  const [isBatchMode, setIsBatchMode] = useState(false);
  const [selectedIds, setSelectedIds] = useState<Set<string>>(new Set());
  const [collapsedGroups, setCollapsedGroups] = useState<Set<string>>(
    new Set(),
  );
  const [archivedGroupOpen, setArchivedGroupOpen] = useState(true);
  const [showTemplatePage, setShowTemplatePage] = useState(false);
  const [detailId, setDetailId] = useState<string | null>(
    notificationAutomationId ?? null,
  );
  const [detailRecordId, setDetailRecordId] = useState<string>();
  const [taskFilter, setTaskFilter] = useState<TaskFilter>("all");
  const [statusUpdating, setStatusUpdating] = useState<Set<string>>(new Set());
  const [backgroundEnabled, setBackgroundEnabled] = useState<boolean | null>(
    null,
  );
  const [backgroundError, setBackgroundError] = useState("");
  const [backgroundReload, setBackgroundReload] = useState(0);
  const [runtimeHelpOpen, setRuntimeHelpOpen] = useState(false);
  const runtimeHelpId = useId();
  useEffect(() => {
    const update = () => setBackgroundReload((value) => value + 1);
    window.addEventListener("echo:desktop-preferences-changed", update);
    window.addEventListener("focus", update);
    return () => {
      window.removeEventListener("echo:desktop-preferences-changed", update);
      window.removeEventListener("focus", update);
    };
  }, []);
  useEffect(() => {
    let live = true;
    void Promise.resolve()
      .then(() => desktopPreferencesGet())
      .then((value) => {
        if (live) {
          setBackgroundEnabled(value.closeToTray);
          setBackgroundError("");
        }
      })
      .catch(() => {
        if (live) setBackgroundError("后台运行设置读取失败");
      });
    return () => {
      live = false;
    };
  }, [backgroundReload]);
  useLayoutEffect(() => {
    if (!notificationAutomationId) return;
    useAutomationEditorStore.getState().suspend();
    setShowTemplatePage(false);
    setIsBatchMode(false);
    setSelectedIds(new Set());
    setConfirmState(null);
    setFocusedAutomationId(notificationAutomationId);
    setDetailId(notificationAutomationId);
    setDetailRecordId(undefined);
    setActiveTab("records");
    setFilterStatus("all");
    setSearchQuery("");
  }, [notificationAutomationId, notificationAutomationSequence]);

  // ---------- 编辑态 ----------
  const editor = useAutomationEditorStore();
  const { isCreating, editingAutomation, draft, visible, operation, setDraft } =
    editor;
  const saving = operation !== null;
  const dialogScope = `${editor.generation}:${visible}`;
  const { confirm: confirmReplaceDraft, dialog: replaceDraftDialog } =
    useAppDialog(dialogScope);

  // ---------- 引用数据（工作空间/模型/技能/专家/连接器） ----------
  const [referenceCwd, setReferenceCwd] = useState(
    () => draft?.cwds.split(",")[0]?.trim() || cwd,
  );
  const [workspaces, setWorkspaces] = useState<WorkspaceInfo[]>([]);
  const [models, setModels] = useState<ModelOption[]>([]);
  const [autoModelId, setAutoModelId] = useState<string>();
  const [modelRuntimeError, setModelRuntimeError] = useState<string>();
  const [skills, setSkills] = useState<SkillInfo[]>([]);
  const [experts, setExperts] = useState<AgentEntry[]>([]);
  const [connectors, setConnectors] = useState<ConnectorOption[]>([]);
  const [referencesLoading, setReferencesLoading] = useState(true);
  const [referenceErrors, setReferenceErrors] = useState<
    Partial<Record<ReferenceCatalog, string>>
  >({});
  const referenceGenerationRef = useRef(0);

  // ---------- 确认弹窗 ----------
  const [confirmState, setConfirmState] = useState<{
    title: string;
    content: string;
    okText: string;
    action: () => Promise<void>;
  } | null>(null);

  const refresh = useCallback(
    async (silent = false, notify = true) => {
      const generation = ++snapshotGenerationRef.current;
      if (!silent) setLoading(true);
      try {
        const nextSnapshot = await automationsSnapshot();
        if (snapshotGenerationRef.current !== generation) return;
        setSnapshot(nextSnapshot);
        setSnapshotError(null);
      } catch (e) {
        if (snapshotGenerationRef.current !== generation) return;
        const message = String(e).replace(/^Error:\s*/, "");
        setSnapshotError(message);
        if (notify) onToast?.(`加载自动化数据失败：${message}`);
      } finally {
        // The latest request owns loading even when a scheduler/focus refresh
        // superseded the initial foreground load.
        if (snapshotGenerationRef.current === generation) setLoading(false);
      }
    },
    [onToast],
  );

  useEffect(() => {
    void refresh();
    return () => {
      snapshotGenerationRef.current += 1;
    };
  }, [refresh]);

  useEffect(() => {
    if (lastRefreshSignalRef.current === refreshSignal) return;
    lastRefreshSignalRef.current = refreshSignal;
    void refresh(true, false);
  }, [refresh, refreshSignal]);

  const hasUnfinishedRuns =
    snapshot?.records.some(
      (record) => record.status === "queued" || record.status === "running",
    ) ?? false;

  useEffect(() => {
    // Lifecycle events are the fast path. Polling remains as recovery for an
    // event emitted while the renderer was suspended or still subscribing.
    const timer = window.setInterval(
      () => {
        void refresh(true, false);
      },
      hasUnfinishedRuns ? 2_000 : 15_000,
    );
    return () => window.clearInterval(timer);
  }, [hasUnfinishedRuns, refresh]);

  useEffect(() => {
    const refreshVisiblePanel = () => {
      if (document.visibilityState === "visible") void refresh(true, false);
    };
    window.addEventListener("focus", refreshVisiblePanel);
    document.addEventListener("visibilitychange", refreshVisiblePanel);
    return () => {
      window.removeEventListener("focus", refreshVisiblePanel);
      document.removeEventListener("visibilitychange", refreshVisiblePanel);
    };
  }, [refresh]);

  const loadReferenceCatalogs = useCallback(async () => {
    const generation = ++referenceGenerationRef.current;
    setReferencesLoading(true);
    const [
      workspaceResult,
      modelResult,
      skillResult,
      expertResult,
      connectorResult,
    ] = await Promise.allSettled([
      agentListWorkspaces(),
      (async () => {
        const auth = await agentAuthStatus();
        return [await providersList(), auth] as const;
      })(),
      skillsList(referenceCwd),
      agentsList(referenceCwd),
      mcpList(),
    ]);
    if (referenceGenerationRef.current !== generation) return;

    const errors: Partial<Record<ReferenceCatalog, string>> = {};
    const captureError = (key: ReferenceCatalog, reason: unknown) => {
      errors[key] = String(reason).replace(/^Error:\s*/, "");
    };
    if (workspaceResult.status === "fulfilled")
      setWorkspaces(workspaceResult.value);
    else captureError("workspaces", workspaceResult.reason);
    if (modelResult.status === "fulfilled") {
      const [catalog, auth] = modelResult.value;
      const options = filterModelsByRuntimeCatalog(
        flattenModels(catalog),
        auth.runtimeModels,
      );
      setModels(options);
      setModelRuntimeError(
        auth.ready
          ? undefined
          : (auth.reason ?? "任务模型尚未就绪，请稍后重试或检查模型连接。"),
      );
      setAutoModelId(
        auth.ready && options.some((model) => model.id === auth.defaultModelId)
          ? auth.defaultModelId
          : undefined,
      );
    } else captureError("models", modelResult.reason);
    if (skillResult.status === "fulfilled")
      setSkills(skillResult.value.filter((skill) => skill.enabled));
    else captureError("skills", skillResult.reason);
    if (expertResult.status === "fulfilled") setExperts(expertResult.value);
    else captureError("experts", expertResult.reason);
    if (connectorResult.status === "fulfilled") {
      setConnectors(
        connectorResult.value.map((connector) => ({
          id: connector.name,
          name: connector.name,
          connected: connector.enabled,
        })),
      );
    } else captureError("connectors", connectorResult.reason);
    setReferenceErrors(errors);
    setReferencesLoading(false);
  }, [referenceCwd]);

  useEffect(() => {
    void loadReferenceCatalogs();
    const refreshCatalogs = () => { void loadReferenceCatalogs(); };
    window.addEventListener("echo:model-catalog-changed", refreshCatalogs);
    window.addEventListener("focus", refreshCatalogs);
    return () => {
      window.removeEventListener("echo:model-catalog-changed", refreshCatalogs);
      window.removeEventListener("focus", refreshCatalogs);
      referenceGenerationRef.current += 1;
    };
  }, [loadReferenceCatalogs]);

  useEffect(() => {
    if (!filterOpen) return;
    const handler = (e: MouseEvent) => {
      if (filterRef.current && !filterRef.current.contains(e.target as Node))
        setFilterOpen(false);
    };
    document.addEventListener("mousedown", handler);
    return () => document.removeEventListener("mousedown", handler);
  }, [filterOpen]);

  // ---------- 创建 / 编辑 ----------
  const requestBegin = useCallback(
    async (nextDraft: AutomationDraft, automation?: Automation) => {
      const current = useAutomationEditorStore.getState();
      if (current.operation) {
        onToast?.("自动化任务正在保存或删除，请稍候再开始其他编辑。");
        return;
      }
      if (automation && current.editingAutomation?.id === automation.id) {
        current.resume();
        return;
      }
      if (automationEditorDirty(current)) {
        const replace = await confirmReplaceDraft({
          title: "替换当前自动化草稿？",
          description:
            "已有未保存的自动化输入。你可以保留草稿继续查看，或舍弃后打开另一个编辑页面。",
          cancelLabel: "保留草稿",
          confirmLabel: "舍弃并继续",
          danger: true,
        });
        if (!replace) return;
        const latest = useAutomationEditorStore.getState();
        if (
          latest.generation !== current.generation ||
          latest.draft !== current.draft ||
          latest.operation
        )
          return;
      }
      useAutomationEditorStore.getState().begin(nextDraft, automation);
      setEditorError(null);
      setReferenceCwd(nextDraft.cwds.split(",")[0]?.trim() || cwd);
      setShowTemplatePage(false);
    },
    [confirmReplaceDraft, onToast, cwd],
  );

  const handleCreate = useCallback(
    (template?: AutomationTemplate) => {
      void requestBegin(buildDraft(template, cwd ?? ""));
    },
    [cwd, requestBegin],
  );

  const handleEdit = useCallback(
    (automation: Automation) => {
      void requestBegin(draftFromAutomation(automation), automation);
    },
    [requestBegin],
  );

  const handleCloseModal = useCallback(() => {
    useAutomationEditorStore.getState().close(editor.generation);
  }, [editor.generation]);

  const { requestClose: requestEditorClose, closeDialog: unsavedDialog } =
    useUnsavedClose({
      dirty: automationEditorDirty(editor),
      busy: saving,
      onClose: handleCloseModal,
      scopeKey: dialogScope,
    });

  const persistDraft = useCallback(
    async (generation: number, runTest: boolean) => {
      const current = useAutomationEditorStore.getState();
      if (
        current.generation !== generation ||
        !current.draft ||
        current.operation
      )
        return;
      const { draft, editingAutomation, isCreating } = current;
      if (
        editingAutomation &&
        (!snapshot ||
          !snapshot.automations.some(
            (item) => item.id === editingAutomation.id,
          ))
      ) {
        onToast?.(
          snapshot
            ? "原自动化任务已被移除，草稿仍保留；请复制所需内容后取消并新建任务。"
            : "任务列表尚未加载，请稍候重试保存。",
        );
        return;
      }
      const message = validateDraft(draft, {
        isCreating,
        existingScheduledAt: editingAutomation
          ? scheduledAtIso({
              scheduledDate: editingAutomation.scheduledDate,
              scheduledTime: editingAutomation.scheduledTime,
            })
          : undefined,
      });
      if (message) {
        setEditorError(message);
        onToast?.(message);
        return;
      }
      const modelMessage = automationModelError(
        draft,
        models,
        autoModelId,
        modelRuntimeError,
        runTest || draft.status === "ACTIVE",
      );
      if (modelMessage) {
        setEditorError(modelMessage);
        onToast?.(modelMessage);
        return;
      }
      if (!current.startOperation("saving", generation)) return;
      try {
        const saved = await automationsSave(
          automationFromDraft(draft, editingAutomation ?? undefined),
        );
        const latest = useAutomationEditorStore.getState();
        if (latest.generation !== generation) return;
        latest.markSaved(saved, draft, generation);
        if (runTest) {
          try {
            await automationsRun(saved.id);
            onToast?.("修改已保存，已安排运行；本次会真实执行任务");
            latest.close(generation);
            setDetailId(saved.id);
          } catch (error) {
            onToast?.(
              `修改已保存，但安排运行失败：${String(error).replace(/^Error:\s*/, "")}`,
            );
          }
        } else {
          latest.close(generation);
          setDetailId(saved.id);
          onToast?.(
            saved.status === "ACTIVE"
              ? `已保存，下次：${nextRunLabel(saved.nextRunAt)}`
              : "已保存，后续计划已暂停",
          );
        }
        await refresh(true);
      } catch (e) {
        setEditorError(String(e).replace(/^Error:\s*/, ""));
        onToast?.(`保存自动化任务失败：${String(e).replace(/^Error:\s*/, "")}`);
      } finally {
        useAutomationEditorStore.getState().finishOperation(generation);
      }
    },
    [snapshot, models, autoModelId, modelRuntimeError, onToast, refresh],
  );

  const handleSave = useCallback(() => {
    void persistDraft(editor.generation, false);
  }, [editor.generation, persistDraft]);

  const handleFallbackToDefault = useCallback(() => {
    setDraft((current) =>
      current ? { ...current, permissionMode: "default" } : current,
    );
  }, []);

  const {
    showConfirmDialog: showPermissionConfirm,
    requestSubmit: requestSaveWithPermissionCheck,
    requestAction: requestActionWithPermissionCheck,
    handleConfirm: handlePermissionConfirm,
    handleCancel: handlePermissionCancel,
    handleFallbackToDefault: handlePermissionFallback,
  } = usePermissionConfirm({
    currentMode: draft?.permissionMode ?? "default",
    initialMode:
      editingAutomation?.permissionMode ??
      (editingAutomation ? "default" : undefined),
    onConfirmedSubmit: handleSave,
    onFallbackToDefault: handleFallbackToDefault,
  });

  useEffect(() => {
    if (!visible) handlePermissionCancel();
  }, [visible, editor.generation, handlePermissionCancel]);

  const handleDelete = useCallback(() => {
    if (!editingAutomation || saving) return;
    const name = editingAutomation.name;
    const id = editingAutomation.id;
    const generation = editor.generation;
    setConfirmState({
      title: `删除 ${name}？`,
      content:
        "此操作将永久删除该自动化任务、舍弃其编辑草稿，并取消所有后续计划，历史运行结果仍保留。",
      okText: "删除自动化任务",
      action: async () => {
        if (
          !useAutomationEditorStore
            .getState()
            .startOperation("deleting", generation)
        )
          return;
        try {
          await automationsDelete(id);
          useAutomationEditorStore.getState().close(generation);
          await refresh(true);
        } catch (e) {
          onToast?.(
            `删除自动化任务失败：${String(e).replace(/^Error:\s*/, "")}`,
          );
        } finally {
          useAutomationEditorStore.getState().finishOperation(generation);
        }
      },
    });
  }, [editingAutomation, editor.generation, saving, onToast, refresh]);

  const handleRowDelete = useCallback(
    (automationId: string) => {
      if (editingAutomation?.id === automationId) {
        handleDelete();
        return;
      }
      const name =
        snapshot?.automations.find((a) => a.id === automationId)?.name || "";
      setConfirmState({
        title: `删除 ${name}？`,
        content:
          "此操作将永久删除该自动化任务并取消所有后续计划，历史运行结果仍保留。",
        okText: "删除自动化任务",
        action: async () => {
          try {
            await automationsDelete(automationId);
            await refresh(true);
          } catch (e) {
            onToast?.(
              `删除自动化任务失败：${String(e).replace(/^Error:\s*/, "")}`,
            );
          }
        },
      });
    },
    [snapshot, editingAutomation, handleDelete, onToast, refresh],
  );

  const handleTogglePause = useCallback(
    async (automationId: string, currentStatus: AutomationStatus) => {
      const current = useAutomationEditorStore.getState();
      const owner =
        current.editingAutomation?.id === automationId ? current : null;
      if (owner && !owner.startOperation("saving", owner.generation)) return;
      setStatusUpdating((ids) => new Set(ids).add(automationId));
      const next: AutomationStatus =
        currentStatus === "ACTIVE" ? "PAUSED" : "ACTIVE";
      try {
        await automationsSetStatus(automationId, next);
        if (owner)
          useAutomationEditorStore
            .getState()
            .syncStatus(next, owner.generation);
        await refresh(true);
      } catch (e) {
        onToast?.(`保存自动化任务失败：${String(e).replace(/^Error:\s*/, "")}`);
      } finally {
        if (owner)
          useAutomationEditorStore.getState().finishOperation(owner.generation);
        setStatusUpdating((ids) => {
          const next = new Set(ids);
          next.delete(automationId);
          return next;
        });
      }
    },
    [onToast, refresh],
  );

  const handleRunTest = useCallback(
    async (automationId: string) => {
      const current = useAutomationEditorStore.getState();
      if (current.editingAutomation?.id === automationId && current.operation)
        return;
      setRunStartingIds((current) => new Set(current).add(automationId));
      try {
        await automationsRun(automationId);
        onToast?.("已安排运行");
        await refresh(true);
      } catch (e) {
        onToast?.(`安排运行失败：${String(e).replace(/^Error:\s*/, "")}`);
      } finally {
        setRunStartingIds((current) => {
          const next = new Set(current);
          next.delete(automationId);
          return next;
        });
      }
    },
    [onToast, refresh],
  );

  const handleResetSession = useCallback(
    (automation: Automation) => {
      setConfirmState({
        title: "重置任务会话上下文？",
        content:
          "下一次运行将创建新的任务专属会话。已有会话和历史结果都会保留；当前运行不受影响。",
        okText: "重置上下文",
        action: async () => {
          try {
            await automationsResetSession(automation.id);
            await refresh(true);
            onToast?.("已重置，下一次运行将开始新的会话");
          } catch (error) {
            onToast?.(`重置失败：${String(error).replace(/^Error:\s*/, "")}`);
          }
        },
      });
    },
    [onToast, refresh],
  );
  const handleStopRun = useCallback(
    async (recordId: string) => {
      try {
        await automationRunStop(recordId);
        await refresh(true);
        onToast?.("已请求停止本次运行，后续计划不受影响");
      } catch (error) {
        await refresh(true);
        onToast?.(`停止尚未确认，可重试：${String(error).replace(/^Error:\s*/, "")}`);
      }
    },
    [onToast, refresh],
  );
  const openTask = useCallback((automation: Automation) => {
    setDetailId(automation.id);
    setDetailRecordId(undefined);
  }, []);
  const openRecord = useCallback((record: AutomationRunRecord) => {
    setDetailId(record.automationId);
    setDetailRecordId(record.id);
  }, []);
  const editorModalRef = useModalFocus<HTMLDivElement>(
    visible,
    requestEditorClose,
  );

  const handleTest = useCallback(() => {
    if (!editingAutomation || !draft || saving) return;
    requestActionWithPermissionCheck(() => {
      void persistDraft(editor.generation, true);
    });
  }, [
    editingAutomation,
    draft,
    saving,
    editor.generation,
    requestActionWithPermissionCheck,
    persistDraft,
  ]);

  // ---------- 运行记录 ----------
  const handleArchiveRecord = useCallback(
    async (itemId: string) => {
      try {
        await automationRecordsArchive(itemId, true);
        await refresh(true);
      } catch (e) {
        onToast?.(`保存自动化任务失败：${String(e).replace(/^Error:\s*/, "")}`);
      }
    },
    [onToast, refresh],
  );

  const handleDeleteRecord = useCallback(
    (itemId: string) => {
      setConfirmState({
        title: "删除该条运行记录？",
        content: "此操作将永久删除该条运行记录。",
        okText: "删除",
        action: async () => {
          try {
            await automationRecordsDelete(itemId);
            await refresh(true);
          } catch (e) {
            onToast?.(
              `删除自动化任务失败：${String(e).replace(/^Error:\s*/, "")}`,
            );
          }
        },
      });
    },
    [onToast, refresh],
  );

  // ---------- 批量管理 ----------
  const handleToggleBatchMode = useCallback(() => {
    setIsBatchMode((prev) => !prev);
    setSelectedIds(new Set());
  }, []);

  const handleBatchDelete = useCallback(() => {
    if (selectedIds.size === 0) return;
    const ids = new Set(selectedIds);
    const current = useAutomationEditorStore.getState();
    const owner =
      current.editingAutomation && ids.has(current.editingAutomation.id)
        ? current
        : null;
    if (owner?.operation) return;
    setConfirmState({
      title: `删除选中的 ${ids.size} 个任务？`,
      content: `此操作将永久删除选中的自动化任务并停止其所有后续运行。${owner ? "其中正在编辑的任务草稿也会被舍弃。" : ""}`,
      okText: "删除",
      action: async () => {
        if (
          owner &&
          !useAutomationEditorStore
            .getState()
            .startOperation("deleting", owner.generation)
        )
          return;
        let failed = 0;
        for (const id of ids) {
          try {
            await automationsDelete(id);
            if (id === owner?.editingAutomation?.id)
              useAutomationEditorStore.getState().close(owner.generation);
          } catch {
            failed += 1;
          }
        }
        if (owner)
          useAutomationEditorStore.getState().finishOperation(owner.generation);
        setSelectedIds(new Set());
        setIsBatchMode(false);
        if (failed > 0)
          onToast?.(
            `已删除 ${ids.size - failed} 个任务，${failed} 个未能删除，请重试`,
          );
        await refresh(true);
      },
    });
  }, [selectedIds, onToast, refresh]);

  // ---------- 派生数据 ----------
  const scheduledAutomations = useMemo(
    () => snapshot?.automations.filter((a) => a.status === "ACTIVE") ?? [],
    [snapshot],
  );
  const pausedAutomations = useMemo(
    () => snapshot?.automations.filter((a) => a.status === "PAUSED") ?? [],
    [snapshot],
  );
  const automationById = useMemo(
    () => new Map((snapshot?.automations ?? []).map((a) => [a.id, a])),
    [snapshot],
  );
  const busyAutomationIds = useMemo(() => {
    const ids = new Set(runStartingIds);
    for (const record of snapshot?.records ?? []) {
      if (record.status === "queued" || record.status === "running") {
        ids.add(record.automationId);
      }
    }
    return ids;
  }, [runStartingIds, snapshot]);
  const { completedItems, archivedItems } = useMemo(() => {
    const completed: AutomationRunRecord[] = [];
    const archived: AutomationRunRecord[] = [];
    for (const item of snapshot?.records ?? []) {
      if (item.archived) archived.push(item);
      else completed.push(item);
    }
    return { completedItems: completed, archivedItems: archived };
  }, [snapshot]);

  const query = searchQuery.trim().toLowerCase();
  const taskCounts = useMemo(() => {
    const counts = {
      all: snapshot?.automations.length ?? 0,
      active: 0,
      paused: 0,
      ended: 0,
    };
    for (const task of snapshot?.automations ?? [])
      counts[taskState(task, snapshot?.records)]++;
    return counts;
  }, [snapshot]);
  const filteredTasks = useMemo(
    () =>
      (snapshot?.automations ?? []).filter(
        (task) =>
          (taskFilter === "all" ||
            taskState(task, snapshot?.records) === taskFilter) &&
          (!query ||
            `${task.name} ${task.prompt} ${task.cwds}`
              .toLowerCase()
              .includes(query)),
      ),
    [snapshot, query, taskFilter],
  );
  const filteredRecords = useMemo(() => {
    let items = [...completedItems, ...archivedItems];
    if (focusedAutomationId)
      items = items.filter((item) => item.automationId === focusedAutomationId);
    if (filterStatus === "success")
      items = items.filter((i) => i.status === "success" && !i.archived);
    else if (filterStatus === "failed")
      items = items.filter((i) => i.status === "failed" && !i.archived);
    else if (filterStatus === "interrupted")
      items = items.filter((i) => i.status === "interrupted" && !i.archived);
    else if (filterStatus === "stopped")
      items = items.filter((i) => i.status === "stopped" && !i.archived);
    else if (filterStatus === "attention")
      items = items.filter((i) => i.needsAttention && runBusy(i) && !i.archived);
    else if (filterStatus === "running") {
      items = items.filter(
        (i) => (i.status === "queued" || i.status === "running") && !i.archived,
      );
    } else if (filterStatus === "archived")
      items = items.filter((i) => i.archived);
    if (query) {
      items = items.filter((i) =>
        (automationById.get(i.automationId)?.name || i.automationName || "")
          .toLowerCase()
          .includes(query),
      );
    }
    return items;
  }, [
    completedItems,
    archivedItems,
    filterStatus,
    query,
    automationById,
    focusedAutomationId,
  ]);

  const groupedRecords = useMemo(() => {
    const groups: { label: string; items: AutomationRunRecord[] }[] = [];
    const groupMap = new Map<string, AutomationRunRecord[]>();
    const now = new Date();
    const today = new Date(now.getFullYear(), now.getMonth(), now.getDate());
    const yesterday = new Date(today.getTime() - 864e5);
    const weekdayByJsDay = [
      DAY_LABELS.SU,
      DAY_LABELS.MO,
      DAY_LABELS.TU,
      DAY_LABELS.WE,
      DAY_LABELS.TH,
      DAY_LABELS.FR,
      DAY_LABELS.SA,
    ];
    const sorted = [...filteredRecords].sort(
      (a, b) =>
        Date.parse(b.finishedAt || b.startedAt) -
        Date.parse(a.finishedAt || a.startedAt),
    );
    for (const item of sorted) {
      if (item.archived) continue;
      const date = new Date(item.finishedAt || item.startedAt);
      const dateDay = new Date(
        date.getFullYear(),
        date.getMonth(),
        date.getDate(),
      );
      let label: string;
      if (dateDay.getTime() === today.getTime()) label = "今天";
      else if (dateDay.getTime() === yesterday.getTime()) label = "昨天";
      else
        label = `${weekdayByJsDay[date.getDay()]} (${date.getMonth() + 1}/${date.getDate()})`;
      if (!groupMap.has(label)) groupMap.set(label, []);
      groupMap.get(label)!.push(item);
    }
    for (const [label, items] of groupMap) groups.push({ label, items });
    return groups;
  }, [filteredRecords]);

  const archivedRecords = useMemo(
    () => filteredRecords.filter((i) => i.archived),
    [filteredRecords],
  );

  const showDefaultTemplateGrid =
    !!snapshot &&
    scheduledAutomations.length === 0 &&
    pausedAutomations.length === 0 &&
    completedItems.length === 0 &&
    archivedItems.length === 0;
  const allTasksEmpty =
    scheduledAutomations.length === 0 && pausedAutomations.length === 0;
  const allRecordsEmpty =
    completedItems.length === 0 && archivedItems.length === 0;
  const isToolbarRightHidden =
    (!showTemplatePage &&
      activeTab === "tasks" &&
      allTasksEmpty &&
      showDefaultTemplateGrid) ||
    (!showTemplatePage && activeTab === "records" && allRecordsEmpty);

  // ============================================================
  // 编辑态渲染（截图 2）
  // ============================================================
  if (visible && (editingAutomation || isCreating) && draft) {
    const referenceErrorEntries = Object.entries(referenceErrors) as Array<
      [ReferenceCatalog, string]
    >;
    return (
      <div className="automation-panel echo-agent-automation atm-editor-host">
        <div className="atm-editor-overlay">
          <div
            className="atm-editor-dialog"
            role="dialog"
            aria-modal="true"
            aria-label={isCreating ? "新建定时任务" : "编辑定时任务"}
            ref={editorModalRef}
          >
            {operation && (
              <div className="atm-background-notice" role="status">
                {operation === "deleting"
                  ? "正在删除自动化任务…"
                  : "正在保存自动化任务…"}
              </div>
            )}
            {editingAutomation &&
              snapshot &&
              !snapshot.automations.some(
                (item) => item.id === editingAutomation.id,
              ) && (
                <div
                  className="atm-background-notice atm-draft-notice"
                  role="alert"
                >
                  <span>
                    原自动化任务已被移除，编辑内容仍保留。可转为新任务草稿，检查后再保存。
                  </span>
                  <button
                    type="button"
                    className="atm-toolbar-btn"
                    disabled={saving}
                    onClick={() =>
                      useAutomationEditorStore.getState().convertToNew()
                    }
                  >
                    转为新任务草稿
                  </button>
                </div>
              )}
            {(referencesLoading || referenceErrorEntries.length > 0) && (
              <div
                className="atm-background-notice"
                role={referenceErrorEntries.length > 0 ? "alert" : "status"}
              >
                <span>
                  {referenceErrorEntries.length > 0
                    ? `部分引用数据加载失败：${referenceErrorEntries.map(([key, message]) => `${REFERENCE_CATALOG_LABELS[key]}：${message}`).join("；")}。已保留其他可用数据。`
                    : "正在加载工作空间、模型与运行时能力…"}
                </span>
                {referenceErrorEntries.length > 0 && (
                  <button
                    type="button"
                    className="atm-toolbar-btn"
                    onClick={() => void loadReferenceCatalogs()}
                    disabled={referencesLoading}
                  >
                    {referencesLoading ? "重试中…" : "重试"}
                  </button>
                )}
              </div>
            )}
            <AutomationEditPage
              mode={isCreating ? "create" : "edit"}
              draft={draft}
              setDraft={(next) =>
                setDraft((current) => {
                  if (!current) return current;
                  return typeof next === "function" ? next(current) : next;
                })
              }
              validationError={editorError}
              saving={saving}
              retrying={
                !!editingAutomation && runStartingIds.has(editingAutomation.id)
              }
              workspaces={workspaces}
              models={models}
              autoModelId={autoModelId}
              skills={skills}
              experts={experts}
              connectors={connectors}
              records={
                editingAutomation
                  ? (snapshot?.records ?? []).filter(
                      (r) => r.automationId === editingAutomation.id,
                    )
                  : []
              }
              createdAt={editingAutomation?.createdAt}
              onSave={requestSaveWithPermissionCheck}
              onClose={requestEditorClose}
              onBack={() => useAutomationEditorStore.getState().suspend()}
              onTest={isCreating ? undefined : handleTest}
              onDelete={isCreating ? undefined : handleDelete}
              onOpenConnectorSettings={() => {
                useAutomationEditorStore.getState().suspend();
                onNavigate?.("专家·技能·连接器");
              }}
              onWorkspaceCommit={(value) =>
                setReferenceCwd(value.split(",")[0]?.trim() || cwd)
              }
              onOpenNotificationSettings={() =>
                onOpenSettings?.("notifications")
              }
              onArchiveRecord={handleArchiveRecord}
              onRetryRecord={handleRunTest}
              onDeleteRecord={handleDeleteRecord}
              onOpenSession={onOpenSession}
            />
          </div>
        </div>
        {unsavedDialog}
        {replaceDraftDialog}
        <AutomationPermissionConfirmDialog
          open={showPermissionConfirm}
          onConfirm={handlePermissionConfirm}
          onCancel={handlePermissionCancel}
          onFallbackToDefault={handlePermissionFallback}
        />
        {confirmState && (
          <ConfirmDialog
            title={confirmState.title}
            content={confirmState.content}
            okText={confirmState.okText}
            danger
            onOk={() => {
              const action = confirmState.action;
              setConfirmState(null);
              void action();
            }}
            onCancel={() => setConfirmState(null)}
          />
        )}
      </div>
    );
  }

  if (detailId && snapshot) {
    return (
      <div className="automation-panel echo-agent-automation">
        <AutomationTaskDetail
          onToast={onToast}
          key={detailId}
          task={automationById.get(detailId)}
          records={snapshot.records.filter(
            (record) => record.automationId === detailId,
          )}
          initialRecordId={detailRecordId}
          starting={busyAutomationIds.has(detailId)}
          onBack={() => {
            setDetailId(null);
            setFocusedAutomationId(null);
            setActiveTab("tasks");
          }}
          onEdit={handleEdit}
          onRun={handleRunTest}
          onPause={(task) => void handleTogglePause(task.id, task.status)}
          onReset={handleResetSession}
          onStop={handleStopRun}
          onDelete={handleRowDelete}
          onArchive={handleArchiveRecord}
          onOpenSession={onOpenSession}
          onSettings={
            onOpenSettings ? () => onOpenSettings("notifications") : undefined
          }
        />
        {snapshotError && (
          <div className="atm-background-notice" role="alert">
            刷新失败：{snapshotError}
            <button
              type="button"
              className="atm-toolbar-btn"
              onClick={() => void refresh()}
            >
              重试
            </button>
          </div>
        )}
        {draft && (
          <div
            className="atm-background-notice atm-draft-notice"
            role="status"
            aria-label="保留的自动化草稿"
          >
            <span>已保留“{draft.name || "未命名任务"}”的未保存草稿。</span>
            <button
              type="button"
              className="atm-toolbar-btn"
              onClick={() => useAutomationEditorStore.getState().resume()}
            >
              继续编辑
            </button>
            <button
              type="button"
              className="atm-toolbar-btn"
              onClick={requestEditorClose}
              disabled={saving}
            >
              舍弃草稿
            </button>
          </div>
        )}
        {unsavedDialog}
        {replaceDraftDialog}
        {confirmState && (
          <ConfirmDialog
            title={confirmState.title}
            content={confirmState.content}
            okText={confirmState.okText}
            danger={confirmState.okText !== "重置上下文"}
            onCancel={() => setConfirmState(null)}
            onOk={() => {
              const action = confirmState.action;
              setConfirmState(null);
              void action();
            }}
          />
        )}
      </div>
    );
  }

  // ============================================================
  // 列表态渲染（截图 1 / 3）
  // ============================================================
  return (
    <div className="automation-panel echo-agent-automation">
      {/* ---------- 工具栏(顶部拖拽条,Tauri 2 需 data-tauri-drag-region) ---------- */}
      {showTemplatePage ? (
        <PageToolbar className="atm-toolbar atm-toolbar--breadcrumb">
          <div className="atm-toolbar-left">
            <div className="atm-detail-breadcrumb">
              <span className="atm-detail-status-icon">
                <AlarmClockIcon className="atm-task-status-icon atm-task-status-icon--scheduled" />
              </span>
              <button
                type="button"
                className="atm-detail-breadcrumb-link"
                onClick={() => setShowTemplatePage(false)}
              >
                定时任务
              </button>
              <span className="atm-detail-breadcrumb-sep">/</span>
              <span className="atm-detail-breadcrumb-current">从模板添加</span>
            </div>
          </div>
        </PageToolbar>
      ) : (
        <PageToolbar className="atm-toolbar">
          <div className="atm-toolbar-left">
            <h1 className="atm-page-title">定时任务</h1>
            {isBatchMode ? (
              <div className="atm-batch-info">
                <button
                  type="button"
                  className="atm-batch-action"
                  onClick={() => {
                    const allIds = filteredTasks.map((a) => a.id);
                    setSelectedIds((prev) =>
                      allIds.length > 0 && allIds.every((id) => prev.has(id))
                        ? new Set()
                        : new Set(allIds),
                    );
                  }}
                >
                  {filteredTasks.length > 0 &&
                  filteredTasks.every((task) => selectedIds.has(task.id))
                    ? "取消"
                    : "全选"}
                </button>
                <button
                  type="button"
                  className="atm-batch-action atm-batch-delete"
                  disabled={selectedIds.size === 0}
                  onClick={handleBatchDelete}
                >
                  删除
                </button>
                <span className="atm-batch-count">
                  已选择
                  <span className="atm-batch-count-num">
                    {selectedIds.size}
                  </span>
                  项
                </span>
              </div>
            ) : (
              <Segmented
                className="atm-tabs"
                value={activeTab}
                onChange={(v) => setActiveTab(v as TabKey)}
                options={[
                  { value: "tasks", label: "任务" },
                  { value: "records", label: "运行记录" },
                ]}
              />
            )}
          </div>
          <div className="atm-toolbar-right">
            {isBatchMode ? (
              <button
                type="button"
                className="atm-toolbar-btn"
                onClick={handleToggleBatchMode}
              >
                退出管理
              </button>
            ) : isToolbarRightHidden ? null : (
              <>
                {activeTab === "records" && (
                  <div className="atm-filter-wrap" ref={filterRef}>
                    <button
                      type="button"
                      aria-label="筛选运行记录"
                      className={`atm-filter-btn${filterStatus !== "all" ? " atm-filter-btn--active" : ""}`}
                      onClick={() => setFilterOpen((v) => !v)}
                    >
                      <svg
                        width="16"
                        height="16"
                        viewBox="0 0 16 16"
                        fill="none"
                        stroke="currentColor"
                        strokeWidth="1.4"
                        strokeLinecap="round"
                      >
                        <path d="M2 4h12M4.5 8h7M6.5 12h3" />
                      </svg>
                      {filterStatus !== "all" && (
                        <span className="atm-filter-dot" />
                      )}
                    </button>
                    {filterOpen && (
                      <div className="atm-filter-menu atm-filter-menu--right">
                        {RECORD_FILTERS.map((f) => (
                          <button
                            key={f.key}
                            type="button"
                            className={`atm-chip-option${filterStatus === f.key ? " active" : ""}`}
                            onClick={() => {
                              setFilterStatus(f.key);
                              setFilterOpen(false);
                            }}
                          >
                            <span className="atm-chip-option-check">
                              {filterStatus === f.key && (
                                <CheckIcon size="sm" />
                              )}
                            </span>
                            <span>{f.label}</span>
                          </button>
                        ))}
                      </div>
                    )}
                  </div>
                )}
                <div className="atm-search-input">
                  <SearchIcon size="sm" className="atm-search-input-icon" />
                  <input
                    type="text"
                    aria-label={
                      activeTab === "tasks" ? "搜索定时任务" : "搜索运行记录"
                    }
                    placeholder={
                      activeTab === "tasks" ? "搜索定时任务" : "搜索运行记录"
                    }
                    value={searchQuery}
                    onChange={(e) => setSearchQuery(e.target.value)}
                  />
                </div>
                {activeTab === "tasks" && (
                  <>
                    <button
                      type="button"
                      className="atm-toolbar-btn"
                      onClick={handleToggleBatchMode}
                    >
                      <AtmBatchManageIcon />
                      <span>批量管理</span>
                    </button>
                    <div className="atm-create-actions">
                      <button
                        type="button"
                        className="atm-toolbar-btn"
                        onClick={() => setShowTemplatePage(true)}
                      >
                        <AtmAddFromTemplateIcon />
                        <span>从模板添加</span>
                      </button>
                      <button
                        type="button"
                        className="atm-toolbar-btn atm-new-task-btn"
                        onClick={() => handleCreate()}
                      >
                        <AddIcon />
                        <span>新建定时任务</span>
                      </button>
                    </div>
                  </>
                )}
              </>
            )}
          </div>
        </PageToolbar>
      )}

      {snapshot && snapshotError && (
        <div className="atm-background-notice" role="alert">
          <span>刷新自动化数据失败：{snapshotError}。已保留上次数据。</span>
          <button
            type="button"
            className="atm-toolbar-btn"
            onClick={() => void refresh()}
            disabled={loading}
          >
            {loading ? "重试中…" : "重试"}
          </button>
        </div>
      )}
      {draft && (
        <div
          className="atm-background-notice atm-draft-notice"
          role="status"
          aria-label="保留的自动化草稿"
        >
          <span>
            {operation
              ? operation === "deleting"
                ? "正在删除自动化任务…"
                : "正在保存自动化任务…"
              : `已保留“${draft.name || "未命名自动化"}”的${automationEditorDirty(editor) ? "未保存草稿" : "编辑页面"}。`}
          </span>
          <div className="atm-draft-notice__actions">
            <button
              type="button"
              className="atm-toolbar-btn"
              onClick={() => useAutomationEditorStore.getState().resume()}
            >
              继续编辑
            </button>
            <button
              type="button"
              className="atm-toolbar-btn"
              onClick={requestEditorClose}
              disabled={saving}
            >
              {automationEditorDirty(editor) ? "舍弃草稿" : "关闭编辑"}
            </button>
          </div>
        </div>
      )}
      {focusedAutomationId && activeTab === "records" && (
        <div className="atm-background-notice" role="status">
          <span>
            正在查看“
            {automationById.get(focusedAutomationId)?.name ?? "关联自动化"}
            ”的运行记录
          </span>
          <button
            type="button"
            className="atm-toolbar-btn"
            onClick={() => setFocusedAutomationId(null)}
          >
            查看全部记录
          </button>
        </div>
      )}

      {/* ---------- 内容 ---------- */}
      {loading && !snapshot ? (
        <div className="atm-panel-empty">加载中…</div>
      ) : snapshotError && !snapshot ? (
        <div className="atm-empty-state" role="alert">
          <div className="atm-empty-state-hero">
            <div className="atm-empty-state-icon">
              <ErrorCircleIcon size={48} />
            </div>
            <div className="atm-empty-state-text">
              自动化数据加载失败：{snapshotError}
            </div>
            <div className="atm-empty-state-actions">
              <button
                type="button"
                className="atm-empty-action-btn"
                onClick={() => void refresh()}
              >
                重试
              </button>
            </div>
          </div>
        </div>
      ) : showTemplatePage ? (
        <div className="atm-template-page">
          <AutomationTemplateGrid
            templates={AUTOMATION_TEMPLATES}
            onSelectTemplate={handleCreate}
          />
        </div>
      ) : activeTab === "tasks" ? (
        allTasksEmpty && showDefaultTemplateGrid ? (
          /* 截图 1：空态 hero + 模板网格 */
          <div className="atm-empty-state">
            <div className="atm-empty-state-hero">
              <div className="atm-empty-state-icon">
                <AutomationEmptyAlarmIcon width={48} height={48} />
              </div>
              <div className="atm-empty-state-text">
                安排任务，让 EchoAgent 按时为你处理
              </div>
              <div className="atm-empty-state-actions">
                <button
                  type="button"
                  className="atm-empty-action-btn"
                  onClick={() => handleCreate()}
                >
                  + 新建定时任务
                </button>
              </div>
            </div>
            <div className="atm-empty-state-templates">
              <h3 className="project-section__title">自动化任务模板</h3>
              <AutomationTemplateGrid
                templates={AUTOMATION_TEMPLATES}
                onSelectTemplate={handleCreate}
              />
            </div>
          </div>
        ) : (
          <div className="atm-task-list atm-task-list--cards">
            <div className="atm-runtime">
              <div className="atm-runtime-bar">
                <div
                  className={`atm-runtime-status${backgroundEnabled === false || backgroundError ? " atm-runtime-status--warn" : ""}`}
                  role="status"
                >
                  <span>本机运行 · {localTimeZoneLabel()}</span>
                  <span>
                    {backgroundError ||
                      (backgroundEnabled === null
                        ? "正在读取后台运行设置…"
                        : `关闭窗口后继续运行：${backgroundEnabled ? "已开启" : "未开启"}`)}
                  </span>
                </div>
                <div className="atm-runtime-actions">
                  {backgroundError && (
                    <button
                      type="button"
                      className="atm-text-button"
                      onClick={() => setBackgroundReload((value) => value + 1)}
                    >
                      重试
                    </button>
                  )}
                  {onOpenSettings && (
                    <button
                      type="button"
                      className="atm-text-button"
                      onClick={() => onOpenSettings("general")}
                    >
                      运行设置
                    </button>
                  )}
                  <button
                    type="button"
                    className="atm-text-button atm-runtime-help-toggle"
                    aria-expanded={runtimeHelpOpen}
                    aria-controls={runtimeHelpId}
                    onClick={() => setRuntimeHelpOpen((value) => !value)}
                  >
                    运行说明
                    <ChevronDownIcon width={12} height={12} />
                  </button>
                </div>
              </div>
              <p
                className="atm-runtime-help"
                id={runtimeHelpId}
                hidden={!runtimeHelpOpen}
              >
                电脑需保持开机且 EchoAgent
                持续运行；休眠、断网或完全退出会影响执行。周期任务恢复后合并补跑一次，一次性任务错过后可手动补跑。
              </p>
            </div>
            <div className="atm-task-filters" aria-label="任务状态筛选">
              {(
                [
                  ["all", "全部"],
                  ["active", "已启用"],
                  ["paused", "已暂停"],
                  ["ended", "已结束"],
                ] as const
              ).map(([key, label]) => (
                <button
                  type="button"
                  key={key}
                  className="atm-task-filter"
                  aria-pressed={taskFilter === key}
                  onClick={() => setTaskFilter(key)}
                >
                  {label}
                  <span>{taskCounts[key]}</span>
                </button>
              ))}
            </div>
            <div className="atm-task-cards">
              {filteredTasks.map((task) => (
                <AutomationTaskCard
                  key={task.id}
                  task={task}
                  records={(snapshot?.records ?? []).filter(
                    (record) => record.automationId === task.id,
                  )}
                  busy={busyAutomationIds.has(task.id)}
                  updating={statusUpdating.has(task.id)}
                  batch={isBatchMode}
                  selected={selectedIds.has(task.id)}
                  onOpen={openTask}
                  onEdit={handleEdit}
                  onRun={handleRunTest}
                  onToggle={handleTogglePause}
                  onSelect={(id) =>
                    setSelectedIds((prev) => {
                      const next = new Set(prev);
                      if (next.has(id)) next.delete(id);
                      else next.add(id);
                      return next;
                    })
                  }
                />
              ))}
            </div>
            {filteredTasks.length === 0 && (
              <div className="atm-panel-empty">没有匹配的定时任务</div>
            )}
          </div>
        )
      ) : allRecordsEmpty || filteredRecords.length === 0 ? (
        /* 截图 3：运行记录空态 */
        <div className="atm-empty-state atm-empty-state--records">
          <div className="atm-empty-state-hero">
            <div className="atm-empty-state-icon">
              <AutomationEmptyRecordsIcon width={48} height={48} />
            </div>
            <div className="atm-empty-state-text">
              {allRecordsEmpty ? "暂无运行记录" : "没有匹配的记录"}
            </div>
          </div>
        </div>
      ) : (
        /* 运行记录列表 */
        <div className="atm-records-list">
          {groupedRecords.map((group) => {
            const isCollapsed = collapsedGroups.has(group.label);
            return (
              <div className="atm-records-group" key={group.label}>
                <button
                  type="button"
                  className="atm-records-group-label"
                  aria-expanded={!isCollapsed}
                  onClick={() =>
                    setCollapsedGroups((prev) => {
                      const next = new Set(prev);
                      if (next.has(group.label)) next.delete(group.label);
                      else next.add(group.label);
                      return next;
                    })
                  }
                >
                  {group.label}
                  <ChevronDownIcon
                    width={14}
                    height={14}
                    className={`atm-records-group-chevron${isCollapsed ? " atm-records-group-chevron--collapsed" : ""}`}
                  />
                </button>
                {!isCollapsed &&
                  group.items.map((item) => (
                    <InboxRow
                      key={item.id}
                      item={item}
                      retrying={runStartingIds.has(item.automationId)}
                      onArchive={handleArchiveRecord}
                      onRetry={handleRunTest}
                      onDelete={handleDeleteRecord}
                      onOpenSession={onOpenSession}
                      onOpenRecord={openRecord}
                    />
                  ))}
              </div>
            );
          })}
          {archivedRecords.length > 0 && (
            <div className="atm-records-group atm-records-group--archived">
              <button
                type="button"
                className="atm-records-group-label"
                aria-expanded={archivedGroupOpen}
                onClick={() => setArchivedGroupOpen((v) => !v)}
              >
                已归档
                <ChevronDownIcon
                  width={14}
                  height={14}
                  className={`atm-records-group-chevron${archivedGroupOpen ? "" : " atm-records-group-chevron--collapsed"}`}
                />
              </button>
              {archivedGroupOpen &&
                archivedRecords.map((item) => (
                  <InboxRow
                    key={item.id}
                    item={item}
                    archived
                    retrying={runStartingIds.has(item.automationId)}
                    onArchive={handleArchiveRecord}
                    onRetry={handleRunTest}
                    onDelete={handleDeleteRecord}
                    onOpenSession={onOpenSession}
                    onOpenRecord={openRecord}
                  />
                ))}
            </div>
          )}
        </div>
      )}

      {unsavedDialog}
      {replaceDraftDialog}
      {confirmState && (
        <ConfirmDialog
          title={confirmState.title}
          content={confirmState.content}
          okText={confirmState.okText}
          danger
          onOk={() => {
            const action = confirmState.action;
            setConfirmState(null);
            void action();
          }}
          onCancel={() => setConfirmState(null)}
        />
      )}
    </div>
  );
}

// ============================================================
// InboxRow — 运行记录行（复刻 EchoAgent InboxRow）
// ============================================================

function InboxRow({
  item,
  archived = false,
  retrying = false,
  onArchive,
  onRetry,
  onDelete,
  onOpenRecord,
}: {
  item: AutomationRunRecord;
  archived?: boolean;
  retrying?: boolean;
  onArchive: (id: string) => void;
  onRetry: (automationId: string) => void;
  onDelete: (id: string) => void;
  onOpenSession?: (sessionId: string, cwd?: string) => void;
  onOpenRecord: (record: AutomationRunRecord) => void;
}) {
  const isUnfinished = item.status === "queued" || item.status === "running";
  const date = new Date(item.finishedAt || item.startedAt);
  const time = date.toLocaleTimeString([], {
    hour: "2-digit",
    minute: "2-digit",
  });
  const dateTime = `${formatRunTime(item.finishedAt || item.startedAt)}`;

  return (
    <div
      className={`atm-row atm-inbox-row${archived ? " atm-archived" : ""}${isUnfinished ? " atm-inbox-row--running" : ""}${item.sessionId ? " atm-inbox-row--openable" : ""}`}
      title={item.error || (item.sessionId ? "打开本次自动化会话" : undefined)}
      role="button"
      tabIndex={0}
      onClick={() => onOpenRecord(item)}
      onKeyDown={(event) => {
        if (
          event.target === event.currentTarget &&
          (event.key === "Enter" || event.key === " ")
        ) {
          event.preventDefault();
          onOpenRecord(item);
        }
      }}
    >
      <div className="atm-row-left">
        <div className="atm-row-content">
          <div className="atm-row-main atm-row-main-inbox">
            <span className="atm-row-name">{item.automationName}</span>
          </div>
          <span
            className="atm-row-result-label"
            title={item.error || runStatusLabel(item)}
          >
            {runStatusLabel(item)}
            {item.error ? ` · ${item.error}` : ""}
            {item.notificationDelivery ? ` · ${item.notificationDelivery}` : ""}
          </span>
        </div>
      </div>
      <div className="atm-row-right">
        <span className="atm-row-right-text">
          <span className="atm-row-time">{archived ? dateTime : time}</span>
          {archived ? (
            <ArchiveIcon
              size={16}
              color="var(--echo-color-text-disabled, #000)"
            />
          ) : (
            <RecordStatusIcon item={item} />
          )}
        </span>
        {!isUnfinished && (
          <div className="atm-row-hover-actions">
            {!archived && item.status === "failed" && (
              <button
                type="button"
                className="atm-row-action-btn"
                title={retrying ? "正在重新运行" : "使用当前任务配置重新运行"}
                aria-label={`重新运行“${item.automationName}”`}
                disabled={retrying}
                onClick={(event) => {
                  event.stopPropagation();
                  onRetry(item.automationId);
                }}
              >
                <PlayIcon width={14} height={14} />
              </button>
            )}
            {!archived && (
              <button
                type="button"
                className="atm-row-archive-btn"
                title="归档"
                onClick={(e) => {
                  e.stopPropagation();
                  onArchive(item.id);
                }}
              >
                <ArchiveIcon width={14} height={14} />
              </button>
            )}
            <button
              type="button"
              className="atm-row-delete-btn"
              title="删除"
              onClick={(e) => {
                e.stopPropagation();
                onDelete(item.id);
              }}
            >
              <DeleteIcon width={14} height={14} />
            </button>
          </div>
        )}
      </div>
    </div>
  );
}
