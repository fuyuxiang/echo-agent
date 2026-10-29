import { findModelChoiceLabel, modelChoiceLabel, modelDisplayName } from "@/lib/model-presentation";
/**
 * 项目详情页四个 tab 面板 — 对齐目标截图，数据来自本地 store。
 *
 *  - 动态: 真实项目会话与资源统计
 *  - 计划/任务: 持久化看板与列表，支持新建、流转和删除
 *  - 资产: 从用户选择的文件复制到项目私有目录，支持打开、新建目录和删除
 */
import { useMemo, useRef, useState, type MouseEvent as ReactMouseEvent } from "react";
import { useProjectsStore, PLAN_COLUMNS, type PlanStatus, type AssetItem } from "@/stores/projects-store";
import { useSessionsStore } from "@/stores/sessions-store";
import { filesystemPickFiles, openLocalPath, projectAssetMakeDir, projectAssetRemove, projectAssetsImport } from "@/lib/agent-client";
import { formatFileSize } from "@/lib/file-utils";
import { useAppDialog } from "./AppDialog";
import { SessionContextMenu } from "./SessionContextMenu";
import { weixinHandoff } from "@/lib/weixin-client";
import type { ModelOption } from "./ModelSelector";
import { MoreDotsIcon } from "@/foundation/components/Icon/icons";
import type { SessionStatus } from "@/lib/types";

function availableModelId(
  models: readonly ModelOption[],
  explicitModelId?: string,
  defaultModelId?: string,
): string | undefined {
  const requested = explicitModelId || defaultModelId;
  return requested && models.some((model) => model.id === requested) ? requested : undefined;
}

function modelLabel(models: readonly ModelOption[], modelId?: string): string {
  if (!modelId) return "未选择模型";
  return findModelChoiceLabel(models, modelId);
}

function ProjectModelSelect({
  label,
  models,
  modelId,
  configuredModelId,
  disabled = false,
  onChange,
}: {
  label: string;
  models: readonly ModelOption[];
  modelId?: string;
  configuredModelId?: string;
  disabled?: boolean;
  onChange: (modelId: string) => void;
}) {
  const unavailable = !!configuredModelId && !models.some((model) => model.id === configuredModelId);
  return (
    <select
      className={`pd-model-select${!modelId ? " pd-model-select--required" : ""}`}
      aria-label={label}
      value={modelId ?? ""}
      disabled={disabled || models.length === 0}
      title={modelId ? modelChoiceLabel(models.find((model) => model.id === modelId) ?? { id: modelId }, models) : unavailable ? `原模型 ${modelDisplayName(configuredModelId)} 已不可用` : "执行前请选择模型"}
      onChange={(event) => {
        if (event.target.value) onChange(event.target.value);
      }}
    >
      <option value="" disabled>
        {models.length === 0 ? "暂无可用模型" : unavailable ? "原模型不可用，请重选" : "请选择模型"}
      </option>
      {models.map((model) => (
        <option key={model.id} value={model.id}>{modelChoiceLabel(model, models)}</option>
      ))}
    </select>
  );
}

// ============================================================
// 动态
// ============================================================

type ActivityStatusFilter =
  | "all"
  | "attention"
  | "running"
  | "pending"
  | "paused"
  | "finished"
  | "unknown";

type ActivityStatusTone =
  | "running"
  | "attention"
  | "pending"
  | "paused"
  | "failed"
  | "finished"
  | "unknown";

interface ActivityStatusMeta {
  label: string;
  tone: ActivityStatusTone;
  filter: Exclude<ActivityStatusFilter, "all" | "attention"> | "attention";
  priority: number;
  description: string;
}

const ACTIVITY_STATUS_META: Record<SessionStatus, ActivityStatusMeta> = {
  awaiting_permission: {
    label: "等待授权",
    tone: "attention",
    filter: "attention",
    priority: 0,
    description: "Agent 正在等待你确认工具执行权限",
  },
  awaiting_answer: {
    label: "等待回答",
    tone: "attention",
    filter: "attention",
    priority: 0,
    description: "Agent 正在等待你回答问题",
  },
  awaiting_approval: {
    label: "等待批准",
    tone: "attention",
    filter: "attention",
    priority: 0,
    description: "Agent 正在等待你批准执行方案",
  },
  failed: {
    label: "执行失败",
    tone: "failed",
    filter: "attention",
    priority: 1,
    description: "最近一轮 Agent 执行失败，请打开对话查看原因",
  },
  working: {
    label: "执行中",
    tone: "running",
    filter: "running",
    priority: 2,
    description: "Agent 正在执行最近一轮任务",
  },
  planning: {
    label: "规划中",
    tone: "running",
    filter: "running",
    priority: 2,
    description: "Agent 正在生成或调整执行方案",
  },
  pausing: {
    label: "正在暂停",
    tone: "running",
    filter: "running",
    priority: 2,
    description: "暂停请求正在生效",
  },
  stopping: {
    label: "正在停止",
    tone: "running",
    filter: "running",
    priority: 2,
    description: "停止请求正在生效",
  },
  pending: {
    label: "未开始",
    tone: "pending",
    filter: "pending",
    priority: 3,
    description: "对话已创建，尚未开始执行",
  },
  paused: {
    label: "已暂停",
    tone: "paused",
    filter: "paused",
    priority: 4,
    description: "最近一轮执行已暂停，可打开对话继续",
  },
  stopped: {
    label: "已停止",
    tone: "paused",
    filter: "paused",
    priority: 4,
    description: "最近一轮执行已停止或因应用重启中断",
  },
  completed: {
    label: "本轮已结束",
    tone: "finished",
    filter: "finished",
    priority: 5,
    description: "最近一轮 Agent 执行已正常结束，不代表项目任务已完成",
  },
};

const UNKNOWN_ACTIVITY_STATUS: ActivityStatusMeta = {
  label: "历史状态未知",
  tone: "unknown",
  filter: "unknown",
  priority: 6,
  description: "该历史记录创建时尚未保存执行状态，可打开对话查看内容",
};

function activityStatusMeta(status?: SessionStatus): ActivityStatusMeta {
  return status ? ACTIVITY_STATUS_META[status] : UNKNOWN_ACTIVITY_STATUS;
}

function effectiveSessionArchived(
  sessionId: string,
  mirrored: boolean | undefined,
  summaries: ReadonlyMap<string, { archived?: boolean }>,
): boolean {
  const summary = summaries.get(sessionId);
  return summary?.archived !== undefined ? !!summary.archived : !!mirrored;
}

const ACTIVITY_FILTER_LABELS: Record<ActivityStatusFilter, string> = {
  all: "全部状态",
  attention: "需处理",
  running: "执行中",
  pending: "未开始",
  paused: "已暂停/停止",
  finished: "本轮已结束",
  unknown: "状态未知",
};

export function ActivityTab({
  projectId,
  onOpenSession,
  onRenameSession,
  onArchiveSession,
  onDeleteSession,
  onToast,
  models = [],
}: {
  projectId: string;
  onOpenSession?: (sessionId: string, cwd?: string) => void;
  onRenameSession?: (sessionId: string, title: string, cwd?: string) => Promise<void>;
  onArchiveSession?: (sessionId: string, archived: boolean, cwd?: string) => Promise<void>;
  onDeleteSession?: (sessionId: string, cwd?: string) => Promise<void>;
  onToast?: (message: string) => void;
  models?: ModelOption[];
}) {
  const project = useProjectsStore((s) => s.projects.find((p) => p.id === projectId));
  const sessionSummaries = useSessionsStore((s) => s.independent);
  const [view, setView] = useState<"active" | "archived">("active");
  const [statusFilter, setStatusFilter] = useState<ActivityStatusFilter>("all");
  const [sortMode, setSortMode] = useState<"priority" | "recent">("priority");
  const [visibleCount, setVisibleCount] = useState(20);
  const [menu, setMenu] = useState<{
    x: number;
    y: number;
    sessionId: string;
    title: string;
    archived: boolean;
    returnFocus?: HTMLElement;
  } | null>(null);
  const skipMenuFocusRestoreRef = useRef(false);
  const { requestConfirmation, dialog } = useAppDialog(projectId);
  const summaryById = useMemo(
    () => new Map(sessionSummaries.map((summary) => [summary.sessionId, summary])),
    [sessionSummaries],
  );
  const allConversations = project?.conversations ?? [];
  const discoverableConversations = useMemo(
    () => allConversations.filter(
      (conversation) => !summaryById.get(conversation.sessionId)?.hidden,
    ),
    [allConversations, summaryById],
  );
  const activeCount = discoverableConversations.filter((conversation) =>
    !effectiveSessionArchived(conversation.sessionId, conversation.archived, summaryById)).length;
  const archivedCount = discoverableConversations.length - activeCount;
  const conversationEntries = useMemo(() => discoverableConversations
    .filter((conversation) => effectiveSessionArchived(
      conversation.sessionId,
      conversation.archived,
      summaryById,
    ) === (view === "archived"))
    .map((conversation) => {
      const summary = summaryById.get(conversation.sessionId);
      return {
        conversation,
        summary,
        archived: effectiveSessionArchived(conversation.sessionId, conversation.archived, summaryById),
        status: activityStatusMeta(summary?.status),
        updatedAt: summary?.updatedAt || conversation.createdAt,
      };
    }), [discoverableConversations, summaryById, view]);
  const statusCounts = useMemo(() => {
    const counts: Record<ActivityStatusFilter, number> = {
      all: conversationEntries.length,
      attention: 0,
      running: 0,
      pending: 0,
      paused: 0,
      finished: 0,
      unknown: 0,
    };
    for (const entry of conversationEntries) counts[entry.status.filter] += 1;
    return counts;
  }, [conversationEntries]);
  const conversations = useMemo(() => conversationEntries
    .filter((entry) => statusFilter === "all" || entry.status.filter === statusFilter)
    .sort((a, b) => {
      if (sortMode === "priority") {
        const priority = a.status.priority - b.status.priority;
        if (priority !== 0) return priority;
      }
      const aTime = new Date(a.updatedAt).getTime() || 0;
      const bTime = new Date(b.updatedAt).getTime() || 0;
      return bTime - aTime;
    }), [conversationEntries, sortMode, statusFilter]);
  const visibleConversations = conversations.slice(0, visibleCount);
  const visibleFilters = (Object.keys(ACTIVITY_FILTER_LABELS) as ActivityStatusFilter[])
    .filter((filter) => filter === "all" || statusCounts[filter] > 0 || filter === statusFilter);

  const switchView = (next: "active" | "archived") => {
    setView(next);
    setStatusFilter("all");
    setVisibleCount(20);
    setMenu(null);
  };

  const selectStatusFilter = (next: ActivityStatusFilter) => {
    setStatusFilter(next);
    setVisibleCount(20);
    setMenu(null);
  };

  const openMenu = (
    event: ReactMouseEvent<HTMLElement>,
    conversation: (typeof allConversations)[number],
    archived: boolean,
    fromPointer: boolean,
  ) => {
    event.preventDefault();
    event.stopPropagation();
    const rect = event.currentTarget.getBoundingClientRect();
    const focused = document.activeElement instanceof HTMLElement
      ? document.activeElement
      : undefined;
    setMenu({
      x: fromPointer ? event.clientX : rect.right - 160,
      y: fromPointer ? event.clientY : rect.bottom + 4,
      sessionId: conversation.sessionId,
      title: conversation.title || "未命名会话",
      archived,
      returnFocus: focused && event.currentTarget.contains(focused)
        ? focused
        : event.currentTarget.querySelector<HTMLElement>("button") ?? undefined,
    });
  };

  const closeMenu = () => {
    const returnFocus = menu?.returnFocus;
    const shouldRestoreFocus = !skipMenuFocusRestoreRef.current;
    skipMenuFocusRestoreRef.current = false;
    setMenu(null);
    if (shouldRestoreFocus) requestAnimationFrame(() => returnFocus?.focus());
  };

  const detachFromProject = (sessionId: string) => {
    const currentProject = useProjectsStore.getState().projects.find((item) => item.id === projectId);
    const linkedItems = [
      ...(currentProject?.plans.filter((item) => item.sessionId === sessionId).map((item) => item.title) ?? []),
      ...(currentProject?.tasks.filter((item) => item.sessionId === sessionId).map((item) => item.title) ?? []),
    ];
    const perform = async () => {
      await useProjectsStore.getState().detachSessionFromProject(projectId, sessionId);
    };
    if (linkedItems.length > 0) {
      skipMenuFocusRestoreRef.current = true;
      requestConfirmation({
        title: `将“${menu?.title || "未命名会话"}”移出项目？`,
        description: `会话关联的工作项“${linkedItems.join("”、“")}”将解除关联。工作项和对话历史都会保留。`,
        confirmLabel: "移出项目",
        returnFocus: menu?.returnFocus,
        action: perform,
      });
      return;
    }
    void perform().catch((error) => onToast?.(`移出项目失败：${String(error).replace(/^Error:\s*/, "")}`));
  };

  const requestDelete = (sessionId: string) => {
    const title = menu?.sessionId === sessionId ? menu.title : "未命名会话";
    skipMenuFocusRestoreRef.current = true;
    requestConfirmation({
      title: `永久删除对话“${title}”？`,
      description: (
        <>
          该对话的全部历史记录和自动生成的会话摘要将被永久删除，且无法恢复。
          项目资产和工作区原始文件不会被删除。
        </>
      ),
      confirmLabel: "永久删除",
      danger: true,
      returnFocus: menu?.returnFocus,
      action: () => onDeleteSession?.(
        sessionId,
        summaryById.get(sessionId)?.cwd || project?.cwd,
      ),
    });
  };

  const renameSession = async (sessionId: string, title: string) => {
    try {
      await onRenameSession?.(sessionId, title, summaryById.get(sessionId)?.cwd || project?.cwd);
    } catch (error) {
      onToast?.(`重命名失败：${String(error).replace(/^Error:\s*/, "")}`);
    }
  };

  const archiveSession = async (sessionId: string, archived: boolean) => {
    try {
      await onArchiveSession?.(
        sessionId,
        archived,
        summaryById.get(sessionId)?.cwd || project?.cwd,
      );
    } catch (error) {
      onToast?.(`${archived ? "归档" : "恢复"}失败：${String(error).replace(/^Error:\s*/, "")}`);
    }
  };

  return (
    <div className="pd-tab">
      <div className="pd-activity-switch" aria-label="项目对话筛选与概览">
        <button
          type="button"
          className={`pd-pill${view === "active" ? " pd-pill--on" : ""}`}
          aria-pressed={view === "active"}
          onClick={() => switchView("active")}
        >
          {activeCount} 个对话
        </button>
        <button
          type="button"
          className={`pd-pill${view === "archived" ? " pd-pill--on" : ""}`}
          aria-pressed={view === "archived"}
          onClick={() => switchView("archived")}
        >
          {archivedCount} 个已归档
        </button>
        <span className="pd-pill pd-pill--stat">{project?.tasks.length ?? 0} 项任务</span>
        <span className="pd-pill pd-pill--stat">{project?.assets.length ?? 0} 个资产</span>
      </div>
      {conversationEntries.length > 0 && (
        <div className="pd-activity-triage">
          <div className="pd-activity-filters" role="group" aria-label="按最近一轮执行状态筛选">
            {visibleFilters.map((filter) => (
              <button
                key={filter}
                type="button"
                className={`pd-status-filter pd-status-filter--${filter}${statusFilter === filter ? " pd-status-filter--on" : ""}`}
                aria-label={`${ACTIVITY_FILTER_LABELS[filter]}，${statusCounts[filter]} 个对话`}
                aria-pressed={statusFilter === filter}
                onClick={() => selectStatusFilter(filter)}
              >
                {filter !== "all" && <span className="pd-status-filter__dot" aria-hidden="true" />}
                <span>{ACTIVITY_FILTER_LABELS[filter]}</span>
                <span className="pd-status-filter__count">{statusCounts[filter]}</span>
              </button>
            ))}
          </div>
          <label className="pd-activity-sort">
            <span>排序</span>
            <select
              aria-label="项目对话排序"
              value={sortMode}
              onChange={(event) => {
                setSortMode(event.target.value as "priority" | "recent");
                setVisibleCount(20);
              }}
            >
              <option value="priority">需处理优先</option>
              <option value="recent">最近更新</option>
            </select>
          </label>
          <p className="pd-activity-status-hint">
            显示对话最近一轮的 Agent 执行状态，不等同于项目任务进度。
          </p>
        </div>
      )}
      {conversationEntries.length === 0 ? (
        <div className="pd-empty">
          {view === "archived"
            ? "还没有已归档的项目对话。"
            : archivedCount > 0
              ? "当前项目对话均已归档，可切换到“已归档”查看或恢复。"
              : "还没有项目对话。在下方输入任务，开始第一次协作。"}
        </div>
      ) : conversations.length === 0 ? (
        <div className="pd-empty pd-empty--filtered">
          <span>没有符合“{ACTIVITY_FILTER_LABELS[statusFilter]}”的对话。</span>
          <button type="button" className="pd-btn" onClick={() => selectStatusFilter("all")}>查看全部状态</button>
        </div>
      ) : (
        <>
          <ul className="pd-task-list" aria-label={view === "archived" ? "已归档项目对话" : "最近项目对话"}>
            {visibleConversations.map(({ conversation, summary, archived, status, updatedAt }) => (
              <li
                key={conversation.sessionId}
                className="pd-conversation-row"
                onContextMenu={(event) => openMenu(event, conversation, archived, true)}
              >
                <button
                  type="button"
                  className={`pd-task-item pd-conversation-row__open${onOpenSession ? " pd-task-item--clickable" : ""}`}
                  onClick={() => onOpenSession?.(
                    conversation.sessionId,
                    summary?.cwd || project?.cwd,
                  )}
                  disabled={!onOpenSession}
                >
                  <span className="pd-task-item__title">{summary?.title || conversation.title}</span>
                  <span className="pd-task-item__meta">
                    <span
                      className={`pd-session-status pd-session-status--${status.tone}`}
                      title={status.description}
                      aria-label={`执行状态：${status.label}。${status.description}`}
                    >
                      <span className="pd-session-status__dot" aria-hidden="true" />
                      {status.label}
                    </span>
                    {archived && <span>已归档</span>}
                    {(summary?.currentModelId || conversation.modelId) && (
                      <span
                        className="pd-conversation-model"
                        title={modelLabel(models, summary?.currentModelId || conversation.modelId)}
                      >
                        {modelLabel(models, summary?.currentModelId || conversation.modelId)}
                      </span>
                    )}
                    <time dateTime={updatedAt}>{relTime(updatedAt)}</time>
                  </span>
                </button>
                <button
                  type="button"
                  className="pd-conversation-row__action"
                  aria-label={`${conversation.title || "未命名会话"}的会话操作`}
                  aria-haspopup="menu"
                  aria-expanded={menu?.sessionId === conversation.sessionId}
                  onClick={(event) => openMenu(event, conversation, archived, false)}
                >
                  <MoreDotsIcon size="sm" />
                </button>
              </li>
            ))}
          </ul>
          {visibleCount < conversations.length && (
            <button
              type="button"
              className="pd-conversation-more"
              onClick={() => setVisibleCount((count) => count + 20)}
            >
              显示更多（还有 {conversations.length - visibleCount} 个）
            </button>
          )}
        </>
      )}
      {menu && (
        <SessionContextMenu
          x={menu.x}
          y={menu.y}
          sessionId={menu.sessionId}
          sessionTitle={menu.title}
          isArchived={menu.archived}
          onClose={closeMenu}
          onRename={onRenameSession ? renameSession : undefined}
          onArchive={onArchiveSession ? archiveSession : undefined}
          onDetach={detachFromProject}
          onDelete={onDeleteSession ? requestDelete : undefined}
          onWeixin={async (sessionId) => {
            try { onToast?.(await weixinHandoff(sessionId)); }
            catch (error) { onToast?.(`微信交接失败：${String(error).replace(/^Error:\s*/, "")}`); }
          }}
        />
      )}
      {dialog}
    </div>
  );
}

// ============================================================
// 计划（看板）
// ============================================================

/** Legacy import compatibility; both views now share the same work items. */
export function PlanTab(props: Parameters<typeof TaskTab>[0]) { return <TaskTab {...props} initialView="board" />; }

export function TaskTab({
  projectId,
  initialView = "list",
  models = [],
  defaultModelId,
  onRun,
  onOpenSession,
  onRestoreSession,
  onToast,
}: {
  projectId: string;
  initialView?: "list" | "board";
  models?: ModelOption[];
  defaultModelId?: string;
  onRun?: (message: string, modelId: string) => Promise<string | undefined>;
  onOpenSession?: (sessionId: string) => void;
  onRestoreSession?: (sessionId: string) => Promise<void>;
  onToast?: (message: string) => void;
}) {
  const tasks = useProjectsStore((s) => s.projects.find((p) => p.id === projectId)?.tasks ?? []);
  const addTask = useProjectsStore((s) => s.addTask);
  const moveTask = useProjectsStore((s) => s.moveTask);
  const setTaskModel = useProjectsStore((s) => s.setTaskModel);
  const linkTaskSession = useProjectsStore((s) => s.linkTaskSession);
  const removeTask = useProjectsStore((s) => s.removeTask);
  const sessionSummaries = useSessionsStore((s) => s.independent);
  const summaryById = useMemo(
    () => new Map(sessionSummaries.map((summary) => [summary.sessionId, summary])),
    [sessionSummaries],
  );
  const runningTaskIdsRef = useRef(new Set<string>());
  const [runningTaskIds, setRunningTaskIds] = useState<Set<string>>(() => new Set());
  const [restoringSessionIds, setRestoringSessionIds] = useState<Set<string>>(() => new Set());
  const [q, setQ] = useState("");
  const [view, setView] = useState(initialView);
  const { requestConfirmation, requestInput, dialog } = useAppDialog(projectId);

  const filtered = tasks.filter((t) => t.title.toLowerCase().includes(q.toLowerCase()));

  const newTask = () => {
    requestInput({
      title: "新建任务",
      fields: [{ name: "title", label: "任务标题", required: true, maxLength: 200 }],
      confirmLabel: "创建",
      action: ({ title }) => addTask(projectId, title.trim()),
    });
  };

  const requestRemoveTask = (task: (typeof tasks)[number]) => {
    requestConfirmation({
      title: `删除任务“${task.title}”？`,
      description: "该任务及其关联信息将从项目中删除。",
      confirmLabel: "删除任务",
      danger: true,
      action: () => removeTask(projectId, task.id),
    });
  };

  const runWithAgent = async (task: (typeof tasks)[number], modelId?: string) => {
    if (!onRun) return;
    if (runningTaskIdsRef.current.has(task.id)) return;
    if (!modelId) {
      onToast?.("请先为该任务选择一个可用模型");
      return;
    }
    runningTaskIdsRef.current.add(task.id);
    setRunningTaskIds(new Set(runningTaskIdsRef.current));
    const previous = task.status;
    setTaskModel(projectId, task.id, modelId);
    moveTask(projectId, task.id, "in_progress");
    try {
      const sessionId = await onRun(`请执行项目任务「${task.title}」。请直接产出可验收结果，如有阻塞请明确说明。`, modelId);
      if (sessionId) linkTaskSession(projectId, task.id, sessionId, modelId);
      else moveTask(projectId, task.id, previous);
    } catch (error) {
      moveTask(projectId, task.id, previous);
      onToast?.(`启动任务失败：${String(error).replace(/^Error:\s*/, "")}`);
    } finally {
      runningTaskIdsRef.current.delete(task.id);
      setRunningTaskIds(new Set(runningTaskIdsRef.current));
    }
  };

  const restoreAndOpen = async (sessionId: string) => {
    if (!onRestoreSession || restoringSessionIds.has(sessionId)) return;
    setRestoringSessionIds((current) => new Set(current).add(sessionId));
    try {
      await onRestoreSession(sessionId);
      onOpenSession?.(sessionId);
    } catch (error) {
      onToast?.(`恢复会话失败：${String(error).replace(/^Error:\s*/, "")}`);
    } finally {
      setRestoringSessionIds((current) => {
        const next = new Set(current);
        next.delete(sessionId);
        return next;
      });
    }
  };

  const renderTask = (t: (typeof tasks)[number]) => {
    const selectedModelId = availableModelId(models, t.modelId, defaultModelId);
    const running = runningTaskIds.has(t.id);
    const sessionSummary = t.sessionId ? summaryById.get(t.sessionId) : undefined;
    const sessionArchived = t.sessionId
      ? effectiveSessionArchived(t.sessionId, t.sessionArchived, summaryById)
      : false;
    const executionStatus = t.sessionId
      ? activityStatusMeta(sessionSummary?.status)
      : undefined;
    return (
      <li className="pd-task-item" key={t.id}>
        <div className="pd-task-item__main">
          <span className="pd-task-item__title">{t.title}</span>
          <span className="pd-task-item__meta">
            {PLAN_COLUMNS.find((column) => column.status === t.status)?.label ?? "待开始"}
            {t.sessionId ? " · 已关联 Agent 会话" : ""}
            {t.sessionId && t.modelId ? ` · ${modelLabel(models, t.modelId)}` : ""}
            {executionStatus && (
              <span
                className={`pd-session-status pd-session-status--${executionStatus.tone}`}
                title={executionStatus.description}
                aria-label={`Agent 执行状态：${executionStatus.label}。${executionStatus.description}`}
              >
                <span className="pd-session-status__dot" aria-hidden="true" />
                {executionStatus.label}
              </span>
            )}
          </span>
        </div>
        <div className="pd-task-item__actions">
          {!t.sessionId && t.status !== "completed" && (
            <ProjectModelSelect
              label={`选择任务模型 ${t.title}`}
              models={models}
              modelId={selectedModelId}
              configuredModelId={t.modelId}
              disabled={running}
              onChange={(modelId) => setTaskModel(projectId, t.id, modelId)}
            />
          )}
          {onRun && !t.sessionId && t.status !== "completed" && (
            <button
              className="pd-btn pd-btn--small"
              onClick={() => void runWithAgent(t, selectedModelId)}
              disabled={!selectedModelId || running}
              title={!selectedModelId ? "请先选择可用模型" : `使用 ${modelLabel(models, selectedModelId)} 执行`}
            >
              {running ? "启动中…" : "交给 Agent"}
            </button>
          )}
          {t.sessionId && onOpenSession && (
            <button
              className="pd-btn pd-btn--small"
              onClick={() => sessionArchived
                ? void restoreAndOpen(t.sessionId!)
                : onOpenSession(t.sessionId!)}
              disabled={sessionArchived && (!onRestoreSession || restoringSessionIds.has(t.sessionId))}
              title={sessionArchived ? "恢复归档会话并继续该任务" : undefined}
            >
              {sessionArchived
                ? restoringSessionIds.has(t.sessionId) ? "恢复中…" : "恢复并打开"
                : "打开会话"}
            </button>
          )}
          <select
            aria-label={`调整任务状态 ${t.title}`}
            value={t.status}
            onChange={(event) => {
              const status = event.target.value as PlanStatus;
              if (status === "completed" && t.status !== "completed") {
                requestConfirmation({ title: `验收「${t.title}」？`, description: "请确认成果符合要求。Agent 本轮结束不会自动完成工作项。", confirmLabel: "确认验收", action: () => moveTask(projectId, t.id, status) });
              } else { moveTask(projectId, t.id, status); }
            }}
            disabled={running}
          >
            {PLAN_COLUMNS.map((column) => <option key={column.status} value={column.status}>{column.label}</option>)}
          </select>
        </div>
        <button
          className="pd-task-item__del"
          aria-label={`删除任务 ${t.title}`}
          onClick={() => requestRemoveTask(t)}
          disabled={running}
        >×</button>
      </li>
    );
  };

  return (
    <div className="pd-tab">
      <div className="pd-toolbar">
        <div className="pd-toolbar__left">
          <span className="pd-toolbar__hint">列表和看板展示同一组工作项；Agent 本轮结束后，请检查成果并确认验收。</span>
        </div>
        <div className="pd-toolbar__right">
          <div role="group" aria-label="工作项视图">
            <button className="pd-btn" aria-pressed={view === "list"} onClick={() => setView("list")}>列表</button>
            <button className="pd-btn" aria-pressed={view === "board"} onClick={() => setView("board")}>看板</button>
          </div>
          <input className="pd-search-inline" aria-label="搜索任务标题" placeholder="搜索任务标题" value={q} onChange={(e) => setQ(e.target.value)} />
          <button className="pd-btn pd-btn--primary" onClick={newTask}>+ 新建任务</button>
        </div>
      </div>

      {filtered.length === 0 ? (
        <div className="pd-empty">{q ? "没有符合条件的任务" : "暂无任务，点击「新建任务」开始。"}</div>
      ) : (
        view === "board" ? <div className="pd-workitems-board">
          {PLAN_COLUMNS.map((column) => <section key={column.status} className="pd-workitems-column" aria-label={column.label}>
            <h3>{column.label} <small>{filtered.filter((item) => item.status === column.status).length}</small></h3>
            <ul className="pd-task-list">{filtered.filter((item) => item.status === column.status).map(renderTask)}</ul>
          </section>)}
        </div> : <ul className="pd-task-list">{filtered.map(renderTask)}</ul>
      )}
      {dialog}
    </div>
  );
}

// ============================================================
// 资产
// ============================================================

function usedBytes(assets: AssetItem[]): number {
  return assets.reduce((sum, asset) => sum + (asset.sizeBytes ?? 0), 0);
}

export function AssetsTab({ projectId, onToast, onUseAsset, selectedAssetIds = [] }: { projectId: string; onToast?: (message: string) => void; onUseAsset?: (asset: AssetItem) => void; selectedAssetIds?: string[] }) {
  const assets = useProjectsStore((s) => s.projects.find((p) => p.id === projectId)?.assets ?? []);
  const addAsset = useProjectsStore((s) => s.addAsset);
  const addAssets = useProjectsStore((s) => s.addAssets);
  const removeAsset = useProjectsStore((s) => s.removeAsset);
  const [q, setQ] = useState("");
  const [uploading, setUploading] = useState(false);
  const { requestConfirmation, requestInput, dialog } = useAppDialog(projectId);

  const used = useMemo(() => usedBytes(assets), [assets]);

  const newFolder = () => {
    requestInput({
      title: "新建项目文件夹",
      description: "文件夹将创建在项目的私有资产目录中。",
      fields: [{ name: "name", label: "文件夹名称", required: true, maxLength: 255 }],
      validate: ({ name }) => validateAssetName(name),
      confirmLabel: "创建",
      action: async ({ name }) => {
        const asset = await projectAssetMakeDir(projectId, name.trim());
        addAsset(projectId, asset);
      },
    });
  };
  const upload = async () => {
    if (uploading) return;
    try {
      setUploading(true);
      const paths = await filesystemPickFiles({ title: "选择要导入项目的文件" });
      if (paths.length === 0) return;
      const imported = await projectAssetsImport(projectId, paths);
      addAssets(projectId, imported);
      onToast?.(`已导入 ${imported.length} 个文件（原文件未修改）`);
    } catch (error) {
      onToast?.(`导入失败：${String(error).replace(/^Error:\s*/, "")}`);
    } finally {
      setUploading(false);
    }
  };

  const remove = (asset: AssetItem) => {
    requestConfirmation({
      title: `删除资产“${asset.name}”？`,
      description: asset.path
        ? "项目私有目录中的副本将被永久删除，原始导入文件不受影响。"
        : "该旧版资产元数据将从项目中删除。",
      confirmLabel: "删除资产",
      danger: true,
      action: async () => {
        if (asset.path) await projectAssetRemove(projectId, asset.path);
        removeAsset(projectId, asset.id);
      },
    });
  };

  const rows = assets.filter((a) => a.name.toLowerCase().includes(q.toLowerCase()));

  return (
    <div className="pd-tab">
      <div className="pd-toolbar">
        <div className="pd-toolbar__left">
          <button className="pd-btn" onClick={newFolder}>新建文件夹</button>
          <button className="pd-btn" onClick={() => void upload()} disabled={uploading}>
            {uploading ? "导入中…" : "导入文件副本"}
          </button>
          <span className="pd-toolbar__hint">
            本地项目资产已用 {formatFileSize(used)}
          </span>
        </div>
        <div className="pd-toolbar__right">
          <input className="pd-search-inline" aria-label="搜索项目资产" placeholder="搜索文件或文件夹…" value={q} onChange={(e) => setQ(e.target.value)} />
        </div>
      </div>

      <p className="project-assets-context-hint">导入的副本会保存在本项目中。选择“用于本次任务”后才会作为附件发送；每个文件最多20MB。</p>

      <table className="pd-asset-table">
        <thead>
          <tr>
            <th className="pd-asset-table__name">名称</th>
            <th>类型</th>
            <th>更新人</th>
            <th>更新时间</th>
            <th>大小</th>
            <th></th>
          </tr>
        </thead>
        <tbody>
          {rows.length === 0 ? (
            <tr>
              <td className="pd-asset-empty" colSpan={6}>暂无资产，点击「导入文件副本」或「新建文件夹」开始。</td>
            </tr>
          ) : (
            rows.map((a) => (
              <tr key={a.id}>
                <td className="pd-asset-table__name">
                  <button
                    type="button"
                    className="pd-asset-open"
                    disabled={!a.path}
                    title={a.path ? "用系统默认应用打开" : "旧版元数据没有对应文件"}
                    onClick={() => a.path && void openLocalPath(a.path).catch((error) => onToast?.(`打开失败：${String(error)}`))}
                  >
                  <span className="pd-asset-icon">{a.kind === "folder" ? "📁" : "📄"}</span>
                  {a.name}
                  </button>
                </td>
                <td>{a.kind === "folder" ? "文件夹" : a.ext ?? "文件"}</td>
                <td>{a.updater ?? "-"}</td>
                <td>{a.updatedAt ? relTime(a.updatedAt) : "-"}</td>
                <td>{a.kind === "folder" ? "-" : a.sizeBytes !== undefined ? formatFileSize(a.sizeBytes) : a.sizeLabel ?? "-"}</td>
                <td>
                  {onUseAsset && a.kind === "file" && a.path && <button type="button" className="pd-btn pd-btn--small" disabled={selectedAssetIds.includes(a.id)} onClick={() => onUseAsset(a)}>{selectedAssetIds.includes(a.id) ? "已选入本次任务" : "用于本次任务"}</button>}
                  <button className="pd-asset-del" aria-label={`删除资产 ${a.name}`} onClick={() => void remove(a)}>×</button>
                </td>
              </tr>
            ))
          )}
        </tbody>
      </table>
      {dialog}
    </div>
  );
}

function validateAssetName(value: string): string | null {
  const name = value.trim();
  if (!name) return "文件夹名称不能为空。";
  if (name === "." || name === ".." || /[\\/\u0000-\u001f\u007f]/.test(name)) {
    return "文件夹名称不能包含路径分隔符或控制字符，也不能是 . 或 ..。";
  }
  return null;
}

function relTime(iso: string): string {
  const diff = Date.now() - new Date(iso).getTime();
  const m = Math.floor(diff / 60000);
  if (m < 1) return "刚刚";
  if (m < 60) return `${m}分钟前`;
  const h = Math.floor(m / 60);
  if (h < 24) return `${h}小时前`;
  return `${Math.floor(h / 24)}天前`;
}
