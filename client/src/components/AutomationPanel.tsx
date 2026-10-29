import { findModelDisplayName } from "@/lib/model-presentation";
/**
 * 自动化面板 — 1:1 复刻 EchoAgent automation-panel/index.tsx。
 *
 * 三态（对应截图 1-3）：
 *  1. 定时任务：顶部 Segmented 页签；空态 hero（闹钟图标 + 「开启你的第一个自动化任务吧」
 *     + 「+ 添加自动化」）+「自动化任务模版」12 模板网格；有任务时按 当前/已暂停 分组列表。
 *  2. 添加/编辑：全页表单（AutomationEditPage）。
 *  3. 运行记录：空态（暂无运行记录）/ 按 今天·昨天·周X 分组的记录列表 + 状态筛选。
 *
 * 数据：automations_snapshot（本地 JSON 存储 + 桌面端后台调度器）。
 */
import { useCallback, useEffect, useId, useLayoutEffect, useMemo, useRef, useState } from "react";
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
  MoreDotsIcon,
  PlayIcon,
  ResumeCircleIcon,
  RunningStatusIcon,
  SearchIcon,
} from "@/foundation/components/Icon/icons";
import {
  agentsList,
  automationRecordsArchive,
  automationRecordsDelete,
  automationsDelete,
  automationsRun,
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
import { AUTOMATION_TEMPLATES, type AutomationTemplate } from "./automation/template-config";
import {
  DAY_LABELS,
  automationFromDraft,
  buildDraft,
  describeSchedule,
  describeValidity,
  draftFromAutomation,
  formatRunTime,
  scheduledAtIso,
  startsInLabel,
  validateDraft,
  type AutomationDraft,
} from "./automation/schedule-utils";
import { Segmented } from "./automation/controls";
import { AutomationTemplateGrid } from "./automation/AutomationTemplateGrid";
import { AutomationEditPage, type ModelOption } from "./automation/AutomationEditPage";
import { AutomationPermissionConfirmDialog } from "./automation/AutomationPermissionConfirmDialog";
import { usePermissionConfirm } from "./automation/usePermissionConfirm";
import type { ConnectorOption } from "./automation/ConnectorSelector";
import { useModalFocus } from "@/lib/use-modal-focus";
import { useUnsavedClose } from "@/lib/use-unsaved-close";
import { useAppDialog } from "./AppDialog";
import { useAutomationEditorStore, automationEditorDirty } from "@/stores/automation-editor-store";

interface AutomationPanelProps {
  onToast?: (msg: string) => void;
  onNavigate?: (label: string) => void;
  onOpenSession?: (sessionId: string, cwd?: string) => void;
  cwd?: string;
  /** Incremented when the native scheduler persists a lifecycle transition. */
  refreshSignal?: number;
  notificationAutomationId?: string;
  notificationAutomationSequence?: number;
}

type TabKey = "tasks" | "records";
type RecordFilter = "all" | "success" | "failed" | "interrupted" | "running" | "archived";
type ReferenceCatalog = "workspaces" | "models" | "skills" | "experts" | "connectors";

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
  { key: "archived", label: "已归档" },
];

function recordStatusLabel(item: AutomationRunRecord): string {
  if (item.completionPending) return "已结束 · 保存待重试";
  if (item.status === "queued") return "等待调度";
  if (item.status === "running") return "运行中";
  if (item.status === "success") return "成功";
  if (item.status === "failed") return "失败";
  if (item.status === "interrupted") return "已人工接管";
  return item.status;
}

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
    return <RunningStatusIcon size={16} color="var(--echo-color-text-disabled, #777)" />;
  }
  if (item.status === "running") {
    return (
      <span className="atm-status-icon-spinning" style={{ display: "inline-flex" }}>
        <RunningStatusIcon size={16} color="#00C29A" />
      </span>
    );
  }
  if (item.status === "success") return <CheckBoldIcon size={16} color="var(--echo-color-text-disabled, #000)" />;
  if (item.status === "failed") return <ErrorCircleIcon size={16} />;
  if (item.status === "interrupted") return <CirclePauseIcon width={16} height={16} color="var(--echo-color-text-secondary)" />;
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
        <h3 id={titleId} className="atm-confirm-title">{title}</h3>
        <p id={contentId} className="atm-confirm-content">{content}</p>
        <div className="atm-confirm-actions">
          <button
            type="button"
            className="atm-btn atm-btn--secondary"
            onClick={onCancel}
            data-modal-initial-focus
          >
            取消
          </button>
          <button type="button" className={`atm-btn ${danger ? "atm-btn--danger" : "atm-btn--primary"}`} onClick={onOk}>
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
}: AutomationPanelProps) {
  // ---------- 数据 ----------
  const [snapshot, setSnapshot] = useState<AutomationSnapshot | null>(null);
  const [loading, setLoading] = useState(true);
  const [snapshotError, setSnapshotError] = useState<string | null>(null);
  const snapshotGenerationRef = useRef(0);
  const lastRefreshSignalRef = useRef(refreshSignal);
  const [runStartingIds, setRunStartingIds] = useState<Set<string>>(new Set());
  const [activeTab, setActiveTab] = useState<TabKey>(notificationAutomationId ? "records" : "tasks");
  const [focusedAutomationId, setFocusedAutomationId] = useState<string | null>(notificationAutomationId ?? null);
  const [searchQuery, setSearchQuery] = useState("");
  const [filterStatus, setFilterStatus] = useState<RecordFilter>("all");
  const [filterOpen, setFilterOpen] = useState(false);
  const filterRef = useRef<HTMLDivElement>(null);
  const [isBatchMode, setIsBatchMode] = useState(false);
  const [selectedIds, setSelectedIds] = useState<Set<string>>(new Set());
  const [collapsedGroups, setCollapsedGroups] = useState<Set<string>>(new Set());
  const [archivedGroupOpen, setArchivedGroupOpen] = useState(true);
  const [showTemplatePage, setShowTemplatePage] = useState(false);

  useLayoutEffect(() => {
    if (!notificationAutomationId) return;
    useAutomationEditorStore.getState().suspend();
    setShowTemplatePage(false);
    setIsBatchMode(false);
    setSelectedIds(new Set());
    setConfirmState(null);
    setFocusedAutomationId(notificationAutomationId);
    setActiveTab("records");
    setFilterStatus("all");
    setSearchQuery("");
  }, [notificationAutomationId, notificationAutomationSequence]);

  // ---------- 编辑态 ----------
  const editor = useAutomationEditorStore();
  const { isCreating, editingAutomation, draft, visible, operation, setDraft } = editor;
  const saving = operation !== null;
  const dialogScope = `${editor.generation}:${visible}`;
  const { confirm: confirmReplaceDraft, dialog: replaceDraftDialog } = useAppDialog(dialogScope);

  // ---------- 引用数据（工作空间/模型/技能/专家/连接器） ----------
  const [workspaces, setWorkspaces] = useState<WorkspaceInfo[]>([]);
  const [models, setModels] = useState<ModelOption[]>([]);
  const [autoModelId, setAutoModelId] = useState<string>();
  const [modelRuntimeError, setModelRuntimeError] = useState<string>();
  const [skills, setSkills] = useState<SkillInfo[]>([]);
  const [experts, setExperts] = useState<AgentEntry[]>([]);
  const [connectors, setConnectors] = useState<ConnectorOption[]>([]);
  const [referencesLoading, setReferencesLoading] = useState(true);
  const [referenceErrors, setReferenceErrors] = useState<Partial<Record<ReferenceCatalog, string>>>({});
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

  const hasUnfinishedRuns = snapshot?.records.some(
    (record) => record.status === "queued" || record.status === "running",
  ) ?? false;

  useEffect(() => {
    // Lifecycle events are the fast path. Polling remains as recovery for an
    // event emitted while the renderer was suspended or still subscribing.
    const timer = window.setInterval(() => {
      void refresh(true, false);
    }, hasUnfinishedRuns ? 2_000 : 15_000);
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
    const [workspaceResult, modelResult, skillResult, expertResult, connectorResult] = await Promise.allSettled([
      agentListWorkspaces(),
      Promise.all([providersList(), agentAuthStatus()]),
      skillsList(cwd),
      agentsList(cwd),
      mcpList(),
    ]);
    if (referenceGenerationRef.current !== generation) return;

    const errors: Partial<Record<ReferenceCatalog, string>> = {};
    const captureError = (key: ReferenceCatalog, reason: unknown) => {
      errors[key] = String(reason).replace(/^Error:\s*/, "");
    };
    if (workspaceResult.status === "fulfilled") setWorkspaces(workspaceResult.value);
    else captureError("workspaces", workspaceResult.reason);
    if (modelResult.status === "fulfilled") {
      const [catalog, auth] = modelResult.value;
      const options = filterModelsByRuntimeCatalog(flattenModels(catalog), auth.runtimeModels);
      setModels(options);
      setModelRuntimeError(auth.ready ? undefined : (auth.reason ?? "任务模型尚未就绪，请稍后重试或检查模型连接。"));
      setAutoModelId(
        auth.ready && options.some((model) => model.id === auth.defaultModelId)
          ? auth.defaultModelId
          : undefined,
      );
    } else captureError("models", modelResult.reason);
    if (skillResult.status === "fulfilled") setSkills(skillResult.value.filter((skill) => skill.enabled));
    else captureError("skills", skillResult.reason);
    if (expertResult.status === "fulfilled") setExperts(expertResult.value);
    else captureError("experts", expertResult.reason);
    if (connectorResult.status === "fulfilled") {
      setConnectors(connectorResult.value.map((connector) => ({
        id: connector.name,
        name: connector.name,
        connected: connector.enabled,
      })));
    } else captureError("connectors", connectorResult.reason);
    setReferenceErrors(errors);
    setReferencesLoading(false);
  }, [cwd]);

  useEffect(() => {
    void loadReferenceCatalogs();
    return () => {
      referenceGenerationRef.current += 1;
    };
  }, [loadReferenceCatalogs]);

  useEffect(() => {
    if (!filterOpen) return;
    const handler = (e: MouseEvent) => {
      if (filterRef.current && !filterRef.current.contains(e.target as Node)) setFilterOpen(false);
    };
    document.addEventListener("mousedown", handler);
    return () => document.removeEventListener("mousedown", handler);
  }, [filterOpen]);

  // ---------- 创建 / 编辑 ----------
  const requestBegin = useCallback(async (nextDraft: AutomationDraft, automation?: Automation) => {
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
        description: "已有未保存的自动化输入。你可以保留草稿继续查看，或舍弃后打开另一个编辑页面。",
        cancelLabel: "保留草稿", confirmLabel: "舍弃并继续", danger: true,
      });
      if (!replace) return;
      const latest = useAutomationEditorStore.getState();
      if (latest.generation !== current.generation || latest.draft !== current.draft || latest.operation) return;
    }
    useAutomationEditorStore.getState().begin(nextDraft, automation);
    setShowTemplatePage(false);
  }, [confirmReplaceDraft, onToast]);

  const handleCreate = useCallback((template?: AutomationTemplate) => {
    void requestBegin(buildDraft(template, cwd ?? ""));
  }, [cwd, requestBegin]);

  const handleEdit = useCallback((automation: Automation) => {
    void requestBegin(draftFromAutomation(automation), automation);
  }, [requestBegin]);

  const handleCloseModal = useCallback(() => {
    useAutomationEditorStore.getState().close(editor.generation);
  }, [editor.generation]);

  const { requestClose: requestEditorClose, closeDialog: unsavedDialog } = useUnsavedClose({
    dirty: automationEditorDirty(editor),
    busy: saving,
    onClose: handleCloseModal,
    scopeKey: dialogScope,
  });

  const persistDraft = useCallback(async (generation: number, runTest: boolean) => {
    const current = useAutomationEditorStore.getState();
    if (current.generation !== generation || !current.draft || current.operation) return;
    const { draft, editingAutomation, isCreating } = current;
    if (editingAutomation && (!snapshot || !snapshot.automations.some((item) => item.id === editingAutomation.id))) {
      onToast?.(snapshot ? "原自动化任务已被移除，草稿仍保留；请复制所需内容后取消并新建任务。" : "任务列表尚未加载，请稍候重试保存。");
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
      onToast?.(message);
      return;
    }
    const modelMessage = automationModelError(draft, models, autoModelId, modelRuntimeError, runTest || draft.status === "ACTIVE");
    if (modelMessage) {
      onToast?.(modelMessage);
      return;
    }
    if (!current.startOperation("saving", generation)) return;
    try {
      const saved = await automationsSave(automationFromDraft(draft, editingAutomation ?? undefined));
      const latest = useAutomationEditorStore.getState();
      if (latest.generation !== generation) return;
      latest.markSaved(saved, draft, generation);
      if (runTest) {
        try {
          await automationsRun(saved.id);
          onToast?.("修改已保存，已安排试运行");
        } catch (error) {
          onToast?.(`修改已保存，但触发测试运行失败：${String(error).replace(/^Error:\s*/, "")}`);
        }
      } else {
        latest.close(generation);
      }
      await refresh(true);
    } catch (e) {
      onToast?.(`保存自动化任务失败：${String(e).replace(/^Error:\s*/, "")}`);
    } finally {
      useAutomationEditorStore.getState().finishOperation(generation);
    }
  }, [snapshot, models, autoModelId, modelRuntimeError, onToast, refresh]);

  const handleSave = useCallback(() => {
    void persistDraft(editor.generation, false);
  }, [editor.generation, persistDraft]);

  const handleFallbackToDefault = useCallback(() => {
    setDraft((current) => (current ? { ...current, permissionMode: "default" } : current));
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
    initialMode: editingAutomation?.permissionMode ?? (editingAutomation ? "default" : undefined),
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
      content: "此操作将永久删除该自动化任务、舍弃其编辑草稿，并停止所有后续运行。",
      okText: "删除自动化任务",
      action: async () => {
        if (!useAutomationEditorStore.getState().startOperation("deleting", generation)) return;
        try {
          await automationsDelete(id);
          useAutomationEditorStore.getState().close(generation);
          await refresh(true);
        } catch (e) {
          onToast?.(`删除自动化任务失败：${String(e).replace(/^Error:\s*/, "")}`);
        } finally {
          useAutomationEditorStore.getState().finishOperation(generation);
        }
      },
    });
  }, [editingAutomation, editor.generation, saving, onToast, refresh]);

  const handleRowDelete = useCallback(
    (automationId: string) => {
      if (editingAutomation?.id === automationId) { handleDelete(); return; }
      const name = snapshot?.automations.find((a) => a.id === automationId)?.name || "";
      setConfirmState({
        title: `删除 ${name}？`,
        content: "此操作将永久删除该自动化任务并停止所有后续运行。",
        okText: "删除自动化任务",
        action: async () => {
          try {
            await automationsDelete(automationId);
            await refresh(true);
          } catch (e) {
            onToast?.(`删除自动化任务失败：${String(e).replace(/^Error:\s*/, "")}`);
          }
        },
      });
    },
    [snapshot, editingAutomation, handleDelete, onToast, refresh],
  );

  const handleTogglePause = useCallback(
    async (automationId: string, currentStatus: AutomationStatus) => {
      const current = useAutomationEditorStore.getState();
      const owner = current.editingAutomation?.id === automationId ? current : null;
      if (owner && !owner.startOperation("saving", owner.generation)) return;
      const next: AutomationStatus = currentStatus === "ACTIVE" ? "PAUSED" : "ACTIVE";
      try {
        await automationsSetStatus(automationId, next);
        if (owner) useAutomationEditorStore.getState().syncStatus(next, owner.generation);
        await refresh(true);
      } catch (e) {
        onToast?.(`保存自动化任务失败：${String(e).replace(/^Error:\s*/, "")}`);
      } finally {
        if (owner) useAutomationEditorStore.getState().finishOperation(owner.generation);
      }
    },
    [onToast, refresh],
  );

  const handleRunTest = useCallback(
    async (automationId: string) => {
      const current = useAutomationEditorStore.getState();
      if (current.editingAutomation?.id === automationId && current.operation) return;
      setRunStartingIds((current) => new Set(current).add(automationId));
      try {
        await automationsRun(automationId);
        onToast?.("已安排试运行");
        await refresh(true);
      } catch (e) {
        onToast?.(`触发测试运行失败：${String(e).replace(/^Error:\s*/, "")}`);
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

  const handleTest = useCallback(() => {
    if (!editingAutomation || !draft || saving) return;
    requestActionWithPermissionCheck(() => {
      void persistDraft(editor.generation, true);
    });
  }, [editingAutomation, draft, saving, editor.generation, requestActionWithPermissionCheck, persistDraft]);

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
            onToast?.(`删除自动化任务失败：${String(e).replace(/^Error:\s*/, "")}`);
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
    const owner = current.editingAutomation && ids.has(current.editingAutomation.id) ? current : null;
    if (owner?.operation) return;
    setConfirmState({
      title: `删除选中的 ${ids.size} 个任务？`,
      content: `此操作将永久删除选中的自动化任务并停止其所有后续运行。${owner ? "其中正在编辑的任务草稿也会被舍弃。" : ""}`,
      okText: "删除",
      action: async () => {
        if (owner && !useAutomationEditorStore.getState().startOperation("deleting", owner.generation)) return;
        let failed = 0;
        for (const id of ids) {
          try {
            await automationsDelete(id);
            if (id === owner?.editingAutomation?.id) useAutomationEditorStore.getState().close(owner.generation);
          } catch {
            failed += 1;
          }
        }
        if (owner) useAutomationEditorStore.getState().finishOperation(owner.generation);
        setSelectedIds(new Set());
        setIsBatchMode(false);
        if (failed > 0) onToast?.(`已删除 ${ids.size - failed} 个任务，${failed} 个未能删除，请重试`);
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
  const filteredScheduled = useMemo(
    () => (query ? scheduledAutomations.filter((a) => a.name.toLowerCase().includes(query)) : scheduledAutomations),
    [scheduledAutomations, query],
  );
  const filteredPaused = useMemo(
    () => (query ? pausedAutomations.filter((a) => a.name.toLowerCase().includes(query)) : pausedAutomations),
    [pausedAutomations, query],
  );
  const filteredRecords = useMemo(() => {
    let items = [...completedItems, ...archivedItems];
    if (focusedAutomationId) items = items.filter((item) => item.automationId === focusedAutomationId);
    if (filterStatus === "success") items = items.filter((i) => i.status === "success" && !i.archived);
    else if (filterStatus === "failed") items = items.filter((i) => i.status === "failed" && !i.archived);
    else if (filterStatus === "interrupted") items = items.filter((i) => i.status === "interrupted" && !i.archived);
    else if (filterStatus === "running") {
      items = items.filter((i) => (i.status === "queued" || i.status === "running") && !i.archived);
    }
    else if (filterStatus === "archived") items = items.filter((i) => i.archived);
    if (query) {
      items = items.filter((i) =>
        (automationById.get(i.automationId)?.name || i.automationName || "").toLowerCase().includes(query),
      );
    }
    return items;
  }, [completedItems, archivedItems, filterStatus, query, automationById, focusedAutomationId]);

  const groupedRecords = useMemo(() => {
    const groups: { label: string; items: AutomationRunRecord[] }[] = [];
    const groupMap = new Map<string, AutomationRunRecord[]>();
    const now = new Date();
    const today = new Date(now.getFullYear(), now.getMonth(), now.getDate());
    const yesterday = new Date(today.getTime() - 864e5);
    const weekdayByJsDay = [DAY_LABELS.SU, DAY_LABELS.MO, DAY_LABELS.TU, DAY_LABELS.WE, DAY_LABELS.TH, DAY_LABELS.FR, DAY_LABELS.SA];
    const sorted = [...filteredRecords].sort(
      (a, b) => Date.parse(b.finishedAt || b.startedAt) - Date.parse(a.finishedAt || a.startedAt),
    );
    for (const item of sorted) {
      if (item.archived) continue;
      const date = new Date(item.finishedAt || item.startedAt);
      const dateDay = new Date(date.getFullYear(), date.getMonth(), date.getDate());
      let label: string;
      if (dateDay.getTime() === today.getTime()) label = "今天";
      else if (dateDay.getTime() === yesterday.getTime()) label = "昨天";
      else label = `${weekdayByJsDay[date.getDay()]} (${date.getMonth() + 1}/${date.getDate()})`;
      if (!groupMap.has(label)) groupMap.set(label, []);
      groupMap.get(label)!.push(item);
    }
    for (const [label, items] of groupMap) groups.push({ label, items });
    return groups;
  }, [filteredRecords]);

  const archivedRecords = useMemo(() => filteredRecords.filter((i) => i.archived), [filteredRecords]);

  const showDefaultTemplateGrid =
    !!snapshot &&
    scheduledAutomations.length === 0 &&
    pausedAutomations.length === 0 &&
    completedItems.length === 0 &&
    archivedItems.length === 0;
  const showTemplateEntryButton = !showDefaultTemplateGrid && !showTemplatePage;
  const allTasksEmpty = scheduledAutomations.length === 0 && pausedAutomations.length === 0;
  const allRecordsEmpty = completedItems.length === 0 && archivedItems.length === 0;
  const isToolbarRightHidden =
    (!showTemplatePage && activeTab === "tasks" && allTasksEmpty && showDefaultTemplateGrid) ||
    (!showTemplatePage && activeTab === "records" && allRecordsEmpty);

  // ============================================================
  // 编辑态渲染（截图 2）
  // ============================================================
  if (visible && (editingAutomation || isCreating) && draft) {
    const referenceErrorEntries = Object.entries(referenceErrors) as Array<[ReferenceCatalog, string]>;
    return (
      <div className="automation-panel echo-agent-automation">
        {operation && <div className="atm-background-notice" role="status">{operation === "deleting" ? "正在删除自动化任务…" : "正在保存自动化任务…"}</div>}
        {editingAutomation && snapshot && !snapshot.automations.some(item => item.id === editingAutomation.id) && (
          <div className="atm-background-notice atm-draft-notice" role="alert">
            <span>原自动化任务已被移除，编辑内容仍保留。可转为新任务草稿，检查后再保存。</span>
            <button type="button" className="atm-toolbar-btn" disabled={saving} onClick={() => useAutomationEditorStore.getState().convertToNew()}>转为新任务草稿</button>
          </div>
        )}
        {(referencesLoading || referenceErrorEntries.length > 0) && (
          <div className="atm-background-notice" role={referenceErrorEntries.length > 0 ? "alert" : "status"}>
            <span>
              {referenceErrorEntries.length > 0
                ? `部分引用数据加载失败：${referenceErrorEntries.map(([key, message]) => `${REFERENCE_CATALOG_LABELS[key]}：${message}`).join("；")}。已保留其他可用数据。`
                : "正在加载工作空间、模型与运行时能力…"}
            </span>
            {referenceErrorEntries.length > 0 && (
              <button type="button" className="atm-toolbar-btn" onClick={() => void loadReferenceCatalogs()} disabled={referencesLoading}>
                {referencesLoading ? "重试中…" : "重试"}
              </button>
            )}
          </div>
        )}
        <AutomationEditPage
          mode={isCreating ? "create" : "edit"}
          draft={draft}
          setDraft={(next) => setDraft((current) => {
            if (!current) return current;
            return typeof next === "function" ? next(current) : next;
          })}
          saving={saving}
          retrying={!!editingAutomation && runStartingIds.has(editingAutomation.id)}
          workspaces={workspaces}
          models={models}
          autoModelId={autoModelId}
          skills={skills}
          experts={experts}
          connectors={connectors}
          records={
            editingAutomation
              ? (snapshot?.records ?? []).filter((r) => r.automationId === editingAutomation.id)
              : []
          }
          createdAt={editingAutomation?.createdAt}
          onSave={requestSaveWithPermissionCheck}
          onClose={requestEditorClose}
          onBack={() => useAutomationEditorStore.getState().suspend()}
          onTest={isCreating ? undefined : handleTest}
          onDelete={isCreating ? undefined : handleDelete}
          onOpenConnectorSettings={() => onNavigate?.("专家·技能·连接器")}
          onArchiveRecord={handleArchiveRecord}
          onRetryRecord={handleRunTest}
          onDeleteRecord={handleDeleteRecord}
          onOpenSession={onOpenSession}
        />
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

  // ============================================================
  // 列表态渲染（截图 1 / 3）
  // ============================================================
  return (
    <div className="automation-panel echo-agent-automation">
      {/* ---------- 工具栏(顶部拖拽条,Tauri 2 需 data-tauri-drag-region) ---------- */}
      {showTemplatePage ? (
        <div className="atm-toolbar atm-toolbar--breadcrumb" data-tauri-drag-region>
          <div className="atm-toolbar-left">
            <div className="atm-detail-breadcrumb">
              <span className="atm-detail-status-icon">
                <AlarmClockIcon className="atm-task-status-icon atm-task-status-icon--scheduled" />
              </span>
              <button type="button" className="atm-detail-breadcrumb-link" onClick={() => setShowTemplatePage(false)}>
                自动化
              </button>
              <span className="atm-detail-breadcrumb-sep">/</span>
              <span className="atm-detail-breadcrumb-current">从模版添加</span>
            </div>
          </div>
        </div>
      ) : (
        <div className="atm-toolbar" data-tauri-drag-region>
          <div className="atm-toolbar-left">
            <h1 className="atm-page-title">自动化</h1>
            {isBatchMode ? (
              <div className="atm-batch-info">
                <button
                  type="button"
                  className="atm-batch-action"
                  onClick={() => {
                    const allIds = [...scheduledAutomations, ...pausedAutomations].map((a) => a.id);
                    setSelectedIds((prev) =>
                      allIds.length > 0 && prev.size === allIds.length ? new Set() : new Set(allIds),
                    );
                  }}
                >
                  {selectedIds.size > 0 && selectedIds.size === scheduledAutomations.length + pausedAutomations.length
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
                  已选择<span className="atm-batch-count-num">{selectedIds.size}</span>项
                </span>
              </div>
            ) : (
              <Segmented
                className="atm-tabs"
                value={activeTab}
                onChange={(v) => setActiveTab(v as TabKey)}
                options={[
                  { value: "tasks", label: "定时任务" },
                  { value: "records", label: "运行记录" },
                ]}
              />
            )}
          </div>
          <div className="atm-toolbar-right">
            {isBatchMode ? (
              <button type="button" className="atm-toolbar-btn" onClick={handleToggleBatchMode}>
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
                      <svg width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round">
                        <path d="M2 4h12M4.5 8h7M6.5 12h3" />
                      </svg>
                      {filterStatus !== "all" && <span className="atm-filter-dot" />}
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
                            <span className="atm-chip-option-check">{filterStatus === f.key && <CheckIcon size="sm" />}</span>
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
                    aria-label="搜索自动化和运行记录"
                    placeholder="搜索自动化/记录"
                    value={searchQuery}
                    onChange={(e) => setSearchQuery(e.target.value)}
                  />
                </div>
                {activeTab === "tasks" && (
                  <>
                    <button type="button" className="atm-toolbar-btn" onClick={handleToggleBatchMode}>
                      <AtmBatchManageIcon />
                      <span>批量管理</span>
                    </button>
                    {showTemplateEntryButton && (
                      <button type="button" className="atm-toolbar-btn" onClick={() => setShowTemplatePage(true)}>
                        <AtmAddFromTemplateIcon />
                        <span>从模版添加</span>
                      </button>
                    )}
                    <button type="button" className="atm-create-btn" onClick={() => handleCreate()}>
                      <AddIcon />
                      <span>添加自动化</span>
                    </button>
                  </>
                )}
              </>
            )}
          </div>
        </div>
      )}

      {snapshot && snapshotError && (
        <div className="atm-background-notice" role="alert">
          <span>刷新自动化数据失败：{snapshotError}。已保留上次数据。</span>
          <button type="button" className="atm-toolbar-btn" onClick={() => void refresh()} disabled={loading}>
            {loading ? "重试中…" : "重试"}
          </button>
        </div>
      )}
      {draft && (
        <div className="atm-background-notice atm-draft-notice" role="status" aria-label="保留的自动化草稿">
          <span>{operation ? (operation === "deleting" ? "正在删除自动化任务…" : "正在保存自动化任务…") : `已保留“${draft.name || "未命名自动化"}”的${automationEditorDirty(editor) ? "未保存草稿" : "编辑页面"}。`}</span>
          <div className="atm-draft-notice__actions">
            <button type="button" className="atm-toolbar-btn" onClick={() => useAutomationEditorStore.getState().resume()}>继续编辑</button>
            <button type="button" className="atm-toolbar-btn" onClick={requestEditorClose} disabled={saving}>{automationEditorDirty(editor) ? "舍弃草稿" : "关闭编辑"}</button>
          </div>
        </div>
      )}
      {focusedAutomationId && activeTab === "records" && (
        <div className="atm-background-notice" role="status">
          <span>正在查看“{automationById.get(focusedAutomationId)?.name ?? "关联自动化"}”的运行记录</span>
          <button type="button" className="atm-toolbar-btn" onClick={() => setFocusedAutomationId(null)}>查看全部记录</button>
        </div>
      )}

      {/* ---------- 内容 ---------- */}
      {loading && !snapshot ? (
        <div className="atm-panel-empty">加载中…</div>
      ) : snapshotError && !snapshot ? (
        <div className="atm-empty-state" role="alert">
          <div className="atm-empty-state-hero">
            <div className="atm-empty-state-icon"><ErrorCircleIcon size={48} /></div>
            <div className="atm-empty-state-text">自动化数据加载失败：{snapshotError}</div>
            <div className="atm-empty-state-actions">
              <button type="button" className="atm-empty-action-btn" onClick={() => void refresh()}>
                重试
              </button>
            </div>
          </div>
        </div>
      ) : showTemplatePage ? (
        <div className="atm-template-page">
          <AutomationTemplateGrid templates={AUTOMATION_TEMPLATES} onSelectTemplate={handleCreate} />
        </div>
      ) : activeTab === "tasks" ? (
        allTasksEmpty && showDefaultTemplateGrid ? (
          /* 截图 1：空态 hero + 模板网格 */
          <div className="atm-empty-state">
            <div className="atm-empty-state-hero">
              <div className="atm-empty-state-icon">
                <AutomationEmptyAlarmIcon width={48} height={48} />
              </div>
              <div className="atm-empty-state-text">开启你的第一个自动化任务吧</div>
              <div className="atm-empty-state-actions">
                <button type="button" className="atm-empty-action-btn" onClick={() => handleCreate()}>
                  + 添加自动化
                </button>
              </div>
            </div>
            <div className="atm-empty-state-templates">
              <h3 className="project-section__title">自动化任务模板</h3>
              <AutomationTemplateGrid templates={AUTOMATION_TEMPLATES} onSelectTemplate={handleCreate} />
            </div>
          </div>
        ) : (
          /* 任务列表：当前 / 已暂停 */
          <div className="atm-task-list">
            {scheduledAutomations.length > 0 && (
              <div className="atm-background-notice" role="status">
                定时任务需要 EchoAgent 持续运行。关闭窗口后能否继续运行取决于「设置 → 系统设置」中的后台运行选项；完全退出会停止任务和调度。有启用任务时会尝试设置开机启动。
              </div>
            )}
            {filteredScheduled.length > 0 && <div className="atm-task-group-label">当前</div>}
            {filteredScheduled.map((automation) => (
              <AutomationRow
                key={automation.id}
                automation={automation}
                isBatchMode={isBatchMode}
                isSelected={selectedIds.has(automation.id)}
                isRunning={busyAutomationIds.has(automation.id)}
                onEdit={handleEdit}
                onRunTest={handleRunTest}
                onTogglePause={handleTogglePause}
                onDelete={handleRowDelete}
                onToggleSelect={(id) =>
                  setSelectedIds((prev) => {
                    const next = new Set(prev);
                    if (next.has(id)) next.delete(id);
                    else next.add(id);
                    return next;
                  })
                }
              />
            ))}
            {filteredPaused.length > 0 && <div className="atm-task-group-label">已暂停</div>}
            {filteredPaused.map((automation) => (
              <AutomationRow
                key={automation.id}
                automation={automation}
                isBatchMode={isBatchMode}
                isSelected={selectedIds.has(automation.id)}
                isRunning={busyAutomationIds.has(automation.id)}
                onEdit={handleEdit}
                onRunTest={handleRunTest}
                onTogglePause={handleTogglePause}
                onDelete={handleRowDelete}
                onToggleSelect={(id) =>
                  setSelectedIds((prev) => {
                    const next = new Set(prev);
                    if (next.has(id)) next.delete(id);
                    else next.add(id);
                    return next;
                  })
                }
              />
            ))}
            {(filteredScheduled.length === 0 && filteredPaused.length === 0) && (
              <div className="atm-panel-empty">没有匹配的自动化</div>
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
            <div className="atm-empty-state-text">{allRecordsEmpty ? "暂无运行记录" : "没有匹配的记录"}</div>
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
// AutomationRow — 任务行（复刻 EchoAgent AutomationRow）
// ============================================================

function AutomationRow({
  automation,
  isBatchMode,
  isSelected,
  isRunning,
  onEdit,
  onRunTest,
  onTogglePause,
  onDelete,
  onToggleSelect,
}: {
  automation: Automation;
  isBatchMode: boolean;
  isSelected: boolean;
  isRunning: boolean;
  onEdit: (a: Automation) => void;
  onRunTest: (id: string) => void;
  onTogglePause: (id: string, status: AutomationStatus) => void;
  onDelete: (id: string) => void;
  onToggleSelect: (id: string) => void;
}) {
  const [menuOpen, setMenuOpen] = useState(false);
  const menuRef = useRef<HTMLDivElement>(null);
  const menuTriggerRef = useRef<HTMLButtonElement>(null);
  const isActive = automation.status === "ACTIVE";
  const scheduleDesc = describeSchedule(automation);
  const validityDesc = describeValidity(automation);
  const projectNames = automation.cwds
    .split(",")
    .map((c) => c.trim())
    .filter(Boolean)
    .map((p) => p.split(/[\\/]/).filter(Boolean).pop() || p);
  const nextLabel = startsInLabel(automation.nextRunAt);

  useEffect(() => {
    if (!menuOpen) return;
    const handler = (e: MouseEvent) => {
      if (menuRef.current && !menuRef.current.contains(e.target as Node)) setMenuOpen(false);
    };
    document.addEventListener("mousedown", handler);
    return () => document.removeEventListener("mousedown", handler);
  }, [menuOpen]);

  return (
    <div
      className={`atm-row${isBatchMode ? " atm-row--batch" : ""}${menuOpen ? " atm-row--menu-open" : ""}`}
      role="group"
      aria-label={`自动化 ${automation.name}`}
    >
      <button
        type="button"
        className="atm-row-left atm-row-open"
        role={isBatchMode ? "checkbox" : undefined}
        aria-label={`${isBatchMode ? "选择" : "编辑"}自动化「${automation.name}」`}
        aria-checked={isBatchMode ? !!isSelected : undefined}
        onClick={() => (isBatchMode ? onToggleSelect(automation.id) : onEdit(automation))}
      >
        {isBatchMode && (
          <span className="atm-row-leading" aria-hidden="true">
            <span className={`echo-checkbox${isSelected ? " echo-checkbox--checked" : ""} atm-row-checkbox`}>
              <span className="echo-checkbox-box">{isSelected && "✓"}</span>
            </span>
          </span>
        )}
        <div className="atm-row-content">
          <div className="atm-row-main">
            <span className="atm-row-name">{automation.name}</span>
          </div>
          <div className="atm-row-meta">
            {projectNames.map((name) => (
              <span className="atm-row-project" title={name} key={`${automation.id}-${name}`}>
                {name}
              </span>
            ))}
            {scheduleDesc && (
              <span className="atm-row-schedule" title={scheduleDesc}>
                {scheduleDesc}
              </span>
            )}
            {validityDesc && (
              <span className="atm-row-validity" title={validityDesc}>
                {validityDesc}
              </span>
            )}
          </div>
        </div>
      </button>
      <div className="atm-row-right">
        <span className="atm-row-right-text">
          {isActive && nextLabel ? (
            <span className="atm-row-next">{nextLabel}</span>
          ) : !isActive ? (
            <span className="atm-row-paused-label">已暂停</span>
          ) : (
            <span className="atm-row-paused-label">暂无后续执行</span>
          )}
        </span>
        {!isBatchMode && (
          <div className="atm-row-hover-actions">
            <button
              type="button"
              className="atm-row-action-btn"
              title={isRunning ? "该任务已在运行" : "测试运行"}
              aria-label={isRunning ? `「${automation.name}」正在运行` : `测试运行「${automation.name}」`}
              disabled={isRunning}
              onClick={(e) => {
                e.stopPropagation();
                onRunTest(automation.id);
              }}
            >
              <PlayIcon width={16} height={16} />
            </button>
            <div className="atm-row-menu-wrap" ref={menuRef}>
              <button
                ref={menuTriggerRef}
                type="button"
                className="atm-row-more-hint"
                aria-label={`打开「${automation.name}」更多操作`}
                aria-expanded={menuOpen}
                onClick={(e) => {
                  e.stopPropagation();
                  setMenuOpen((v) => !v);
                }}
              >
                <MoreDotsIcon width={16} height={16} />
              </button>
              {menuOpen && (
                <div className="atm-row-menu" onClick={(e) => e.stopPropagation()}>
                  <button
                    type="button"
                    className="atm-row-menu-item"
                    onClick={() => {
                      setMenuOpen(false);
                      onTogglePause(automation.id, automation.status);
                    }}
                  >
                    {isActive ? <CirclePauseIcon width={14} height={14} /> : <ResumeCircleIcon width={14} height={14} />}
                    <span>{isActive ? "暂停" : "恢复"}</span>
                  </button>
                  <button
                    type="button"
                    className="atm-row-menu-item atm-row-menu-item--danger"
                    onClick={() => {
                      // Keep a connected, meaningful restore target before the
                      // menu item is removed and the confirmation dialog mounts.
                      menuTriggerRef.current?.focus();
                      setMenuOpen(false);
                      onDelete(automation.id);
                    }}
                  >
                    <DeleteIcon width={14} height={14} />
                    <span>删除</span>
                  </button>
                </div>
              )}
            </div>
          </div>
        )}
      </div>
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
  onOpenSession,
}: {
  item: AutomationRunRecord;
  archived?: boolean;
  retrying?: boolean;
  onArchive: (id: string) => void;
  onRetry: (automationId: string) => void;
  onDelete: (id: string) => void;
  onOpenSession?: (sessionId: string, cwd?: string) => void;
}) {
  const isUnfinished = item.status === "queued" || item.status === "running";
  const date = new Date(item.finishedAt || item.startedAt);
  const time = date.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
  const dateTime = `${formatRunTime(item.finishedAt || item.startedAt)}`;

  return (
    <div
      className={`atm-row atm-inbox-row${archived ? " atm-archived" : ""}${isUnfinished ? " atm-inbox-row--running" : ""}${item.sessionId ? " atm-inbox-row--openable" : ""}`}
      title={item.error || (item.sessionId ? "打开本次自动化会话" : undefined)}
      role={item.sessionId ? "button" : undefined}
      tabIndex={item.sessionId ? 0 : undefined}
      onClick={() => item.sessionId && onOpenSession?.(item.sessionId, item.cwd)}
      onKeyDown={(event) => {
        if (item.sessionId && (event.key === "Enter" || event.key === " ")) {
          event.preventDefault();
          onOpenSession?.(item.sessionId, item.cwd);
        }
      }}
    >
      <div className="atm-row-left">
        <div className="atm-row-content">
          <div className="atm-row-main atm-row-main-inbox">
            <span className="atm-row-name">{item.automationName}</span>
          </div>
          <span className="atm-row-result-label" title={item.error || recordStatusLabel(item)}>
            {recordStatusLabel(item)}
            {item.error ? ` · ${item.error}` : ""}
            {item.notificationDelivery ? ` · ${item.notificationDelivery}` : ""}
          </span>
        </div>
      </div>
      <div className="atm-row-right">
        <span className="atm-row-right-text">
          <span className="atm-row-time">{archived ? dateTime : time}</span>
          {archived ? (
            <ArchiveIcon size={16} color="var(--echo-color-text-disabled, #000)" />
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
