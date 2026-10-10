/**
 * 计划面板 — 显示 EchoAgent 的 ACP Plan（任务列表）+ 审批/编辑/执行控制。
 *
 * 增强点（对齐 EchoAgent）：
 *  - Plan 审批流程：planMode 开启时显示 Approve / Reject / Edit 按钮
 *  - Plan 编辑：修改 entry 文本、优先级、删除
 *  - 任务执行控制：跳过 / 重试单个任务
 *  - 进度追踪：每个 in_progress 任务显示耗时
 *  - Plan mode 开关按钮
 */
import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import { useSessionStore } from "@/stores/session-store";
import {
  agentResolvePlanApproval,
  setPlanMode as agentSetPlanMode,
} from "@/lib/agent-client";
import type { PlanEntry, PlanEntryPriority, PlanEntryStatus } from "@/lib/types";
import {
  reorderPlan,
  addPlanEntry,
  cycleEntryStatus,
  planRevisionPrompt,
} from "@/lib/plan-utils";
import {
  CheckIcon,
  ClockIcon,
  LoaderIcon,
  TaskListIcon,
  DeleteIcon,
  ArrowUpIcon,
  ChevronDownIcon,
  AddIcon,
  XCloseIcon,
} from "@/foundation/components/Icon/icons";

const STATUS_LABEL: Record<PlanEntryStatus, string> = {
  pending: "待处理",
  in_progress: "进行中",
  completed: "已完成",
};

const PRIORITY_LABEL: Record<PlanEntryPriority, string> = {
  high: "高",
  medium: "中",
  low: "低",
};

const PRIORITY_CYCLE: PlanEntryPriority[] = ["high", "medium", "low"];

interface PlanPanelProps {
  sessionId?: string;
  onSend?: (text: string) => boolean | void | Promise<boolean | void>;
  onToast?: (msg: string) => void;
  /** Keep an owning workflow (such as Echo Code) in sync with runtime approval. */
  onApprovalResolved?: (
    outcome: "approved" | "cancelled" | "abandoned",
    entries: string[],
  ) => void | Promise<void>;
  /** Stop execution if runtime approval succeeded but workflow persistence failed. */
  onApprovalSyncFailed?: (reason: string) => void | Promise<void>;
}

export function PlanPanel({
  sessionId,
  onSend,
  onToast,
  onApprovalResolved,
  onApprovalSyncFailed,
}: PlanPanelProps) {
  const plan = useSessionStore((s) => s.plan);
  const planMode = useSessionStore((s) => s.planMode);
  const planApproval = useSessionStore((s) => s.planApproval);
  const dismissPlanApproval = useSessionStore((s) => s.dismissPlanApproval);
  const setPlan = useSessionStore((s) => s.setPlan);
  const markPlanRevisionSynced = useSessionStore((s) => s.markPlanRevisionSynced);
  const dirty = useSessionStore((s) => sessionId
    ? s.transcripts[sessionId]?.planRevisionDirty ?? false
    : s.planRevisionDirty);

  const [editingIdx, setEditingIdx] = useState<number | null>(null);
  const [editText, setEditText] = useState("");
  const [elapsed, setElapsed] = useState<Record<number, number>>({});
  const [approvalBusy, setApprovalBusy] = useState(false);
  const [approvalError, setApprovalError] = useState<string | null>(null);
  const timerRef = useRef<ReturnType<typeof setInterval> | null>(null);
  // Track when each task started (for elapsed time display).
  const startTimesRef = useRef<Record<number, number>>({});
  const [newStep, setNewStep] = useState("");
  const sessionEpochRef = useRef(0);
  const currentSessionRef = useRef(sessionId);
  currentSessionRef.current = sessionId;
  const approvalPendingRef = useRef(false);
  const currentApprovalRef = useRef(planApproval?.requestId);
  currentApprovalRef.current = planApproval?.requestId;

  useLayoutEffect(() => {
    sessionEpochRef.current += 1;
    approvalPendingRef.current = false;
    setEditingIdx(null);
    setEditText("");
    setNewStep("");
    setElapsed({});
    startTimesRef.current = {};
    setApprovalBusy(false);
    setApprovalError(null);
    return () => { sessionEpochRef.current += 1; };
  }, [sessionId]);

  // Elapsed timer: tick every second while there are in_progress tasks.
  useEffect(() => {
    if (!plan) return;
    const hasActive = plan.entries.some((e) => e.status === "in_progress");
    if (hasActive && !timerRef.current) {
      timerRef.current = setInterval(() => {
        const now = Date.now();
        const next: Record<number, number> = {};
        plan.entries.forEach((e, i) => {
          if (e.status === "in_progress") {
            if (!startTimesRef.current[i]) startTimesRef.current[i] = now;
            next[i] = Math.round((now - startTimesRef.current[i]) / 1000);
          }
        });
        setElapsed(next);
      }, 1000);
    } else if (!hasActive && timerRef.current) {
      clearInterval(timerRef.current);
      timerRef.current = null;
      setElapsed({});
    }
    return () => {
      if (timerRef.current) {
        clearInterval(timerRef.current);
        timerRef.current = null;
      }
    };
  }, [plan]);

  // Reset start times when plan changes structurally.
  useEffect(() => {
    startTimesRef.current = {};
    setElapsed({});
  }, [plan?.entries.length]);

  useEffect(() => {
    approvalPendingRef.current = false;
    setApprovalBusy(false);
    setApprovalError(null);
  }, [planApproval?.requestId]);

  const handleTogglePlanMode = useCallback(async () => {
    if (!sessionId) return;
    try {
      await agentSetPlanMode(sessionId, !planMode);
      onToast?.(planMode ? "已请求退出计划模式" : "已请求进入计划模式");
    } catch (e) {
      onToast?.(`切换失败：${String(e).replace(/^Error:\s*/, "")}`);
    }
  }, [sessionId, planMode, onToast]);

  const resolveApproval = useCallback(async (
    outcome: "approved" | "cancelled" | "abandoned",
    feedback?: string,
  ) => {
    if (!planApproval || approvalPendingRef.current) return false;
    const epoch = sessionEpochRef.current;
    const requestId = planApproval.requestId;
    const isCurrentSession = () => currentSessionRef.current === sessionId
      && sessionEpochRef.current === epoch
      && currentApprovalRef.current === requestId;
    approvalPendingRef.current = true;
    setApprovalBusy(true);
    setApprovalError(null);
    let runtimeAcknowledged = false;
    try {
      const acknowledged = await agentResolvePlanApproval(
        planApproval.requestId,
        outcome,
        feedback,
      );
      if (!acknowledged) throw new Error("后端未找到该计划审批请求，请重试");
      runtimeAcknowledged = true;
      await onApprovalResolved?.(
        outcome,
        plan?.entries.map((entry) => entry.content.trim()).filter(Boolean) ?? [],
      );
      dismissPlanApproval(planApproval.requestId, planApproval.sessionId);
      if (plan) markPlanRevisionSynced(plan, planApproval.sessionId);
      return true;
    } catch (error) {
      const message = String(error).replace(/^Error:\s*/, "");
      if (runtimeAcknowledged) {
        await Promise.resolve(onApprovalSyncFailed?.(message)).catch(() => undefined);
      }
      if (isCurrentSession()) setApprovalError(message);
      onToast?.(`审批失败：${message}`);
      return false;
    } finally {
      if (isCurrentSession()) {
        approvalPendingRef.current = false;
        setApprovalBusy(false);
      }
    }
  }, [
    dismissPlanApproval,
    markPlanRevisionSynced,
    onApprovalResolved,
    onApprovalSyncFailed,
    onToast,
    plan,
    planApproval,
    sessionId,
  ]);

  const syncPlan = useCallback(async (execute: boolean) => {
    if (!plan) return;
    try {
      if (planApproval) {
        const accepted = execute
          ? await resolveApproval("approved")
          : await resolveApproval("cancelled", planRevisionPrompt(plan, false));
        if (accepted) {
          onToast?.(execute ? "计划已批准，开始执行" : "修订已送回 Agent 继续规划");
        }
        return;
      }
      if (!onSend) return;
      const accepted = await onSend(planRevisionPrompt(plan, execute));
      if (accepted === false) {
        onToast?.("当前会话正在工作，修订计划尚未同步");
        return;
      }
      markPlanRevisionSynced(plan, sessionId);
      onToast?.(execute ? "修订计划已同步，开始执行" : "修订计划已同步到 Agent");
    } catch (error) {
      onToast?.(`同步失败：${String(error).replace(/^Error:\s*/, "")}`);
    }
  }, [markPlanRevisionSynced, onSend, onToast, plan, planApproval, resolveApproval, sessionId]);

  const updateLocalPlan = useCallback((next: NonNullable<typeof plan>) => {
    setPlan(next, { sessionId, localRevision: true });
  }, [sessionId, setPlan]);

  const handleApprove = useCallback(() => void resolveApproval("approved"), [resolveApproval]);

  const handleRevise = useCallback(() => {
    const feedback = plan
      ? planRevisionPrompt(plan, false)
      : "请继续完善计划，暂不开始执行。";
    void resolveApproval("cancelled", feedback);
  }, [plan, resolveApproval]);

  const handleAbandon = useCallback(() => {
    void resolveApproval("abandoned");
  }, [resolveApproval]);

  const handleSkip = useCallback(
    (idx: number) => {
      if (!plan) return;
      const entries = plan.entries.map((e, i) =>
        i === idx ? { ...e, status: "completed" as PlanEntryStatus } : e
      );
      updateLocalPlan({ ...plan, entries });
      onToast?.(`已跳过 #${idx + 1}`);
    },
    [plan, updateLocalPlan, onToast],
  );

  const handleDeleteEntry = useCallback(
    (idx: number) => {
      if (!plan) return;
      const entries = plan.entries.filter((_, i) => i !== idx);
      updateLocalPlan({ ...plan, entries });
      onToast?.(`已删除 #${idx + 1}`);
    },
    [plan, updateLocalPlan, onToast],
  );

  // 计划编辑器(对齐 EchoAgent plan-editor):上移/下移/状态循环/新增步骤。
  const handleMove = useCallback(
    (idx: number, dir: -1 | 1) => {
      if (!plan) return;
      updateLocalPlan(reorderPlan(plan, idx, idx + dir));
    },
    [plan, updateLocalPlan],
  );
  const handleCycleStatus = useCallback(
    (idx: number) => {
      if (!plan) return;
      updateLocalPlan(cycleEntryStatus(plan, idx));
    },
    [plan, updateLocalPlan],
  );
  const handleAddStep = useCallback(() => {
    if (!plan || !newStep.trim()) return;
    updateLocalPlan(addPlanEntry(plan, newStep));
    setNewStep("");
    onToast?.("已新增步骤");
  }, [plan, newStep, updateLocalPlan, onToast]);

  const handleCyclePriority = useCallback(
    (idx: number) => {
      if (!plan) return;
      const entries = plan.entries.map((e, i) => {
        if (i !== idx) return e;
        const cur = PRIORITY_CYCLE.indexOf(e.priority);
        const next = PRIORITY_CYCLE[(cur + 1) % PRIORITY_CYCLE.length];
        return { ...e, priority: next };
      });
      updateLocalPlan({ ...plan, entries });
    },
    [plan, updateLocalPlan],
  );

  const handleSaveEdit = useCallback(
    (idx: number) => {
      if (!plan || !editText.trim()) {
        setEditingIdx(null);
        return;
      }
      const entries = plan.entries.map((e, i) =>
        i === idx ? { ...e, content: editText.trim() } : e
      );
      updateLocalPlan({ ...plan, entries });
      setEditingIdx(null);
    },
    [plan, editText, updateLocalPlan],
  );

  const approvalActions = planApproval ? (
    <div className="plan-panel__approval">
      <button
        className="plan-panel__approve-btn"
        onClick={handleApprove}
        disabled={approvalBusy}
      >
        <CheckIcon size="sm" /> 批准执行
      </button>
      <button
        className="plan-panel__reject-btn"
        onClick={handleRevise}
        disabled={approvalBusy}
      >
        修改计划
      </button>
      <button
        className="plan-panel__reject-btn"
        onClick={handleAbandon}
        disabled={approvalBusy}
      >
        <XCloseIcon size="sm" /> 放弃计划
      </button>
      {approvalError && <p role="alert">{approvalError}</p>}
    </div>
  ) : null;

  // Empty states
  if ((planMode || planApproval) && (!plan || plan.entries.length === 0)) {
    return (
      <div className="plan-panel plan-panel--empty">
        <TaskListIcon size="xl" color="var(--echo-text-tertiary)" />
        <p>计划模式已开启</p>
        <p className="plan-panel__hint">
          发送一个任务，EchoAgent 会先制定计划再执行。
        </p>
        {planApproval?.planContent && (
          <pre className="plan-panel__approval-content">{planApproval.planContent}</pre>
        )}
        {approvalActions}
        <button className="plan-panel__mode-btn" onClick={handleTogglePlanMode}>
          退出计划模式
        </button>
      </div>
    );
  }

  if (!plan || plan.entries.length === 0) {
    return (
      <div className="plan-panel plan-panel--empty">
        <TaskListIcon size="xl" color="var(--echo-text-tertiary)" />
        <p>暂无任务计划</p>
        <p className="plan-panel__hint">
          EchoAgent 在处理复杂任务时会自动制定计划。
        </p>
      </div>
    );
  }

  const completed = plan.entries.filter((e) => e.status === "completed").length;
  const inProgress = plan.entries.filter((e) => e.status === "in_progress").length;
  const total = plan.entries.length;
  const progressPct = total > 0 ? Math.round((completed / total) * 100) : 0;
  const allDone = completed === total;
  const awaitingApproval = planApproval != null;

  return (
    <div className="plan-panel">
      <div className="plan-panel__header">
        <h3 className="plan-panel__title">
          <TaskListIcon size="sm" /> 执行计划
        </h3>
        <div className="plan-panel__header-actions">
          {dirty && (
            <button
              className="plan-panel__sync-btn"
              onClick={() => void syncPlan(false)}
              title="将当前编辑后的计划发送给 Agent"
            >
              同步修订
            </button>
          )}
          <span className="plan-panel__progress-text">
            {completed}/{total}
          </span>
          <button
            className={`plan-panel__mode-toggle ${planMode ? "plan-panel__mode-toggle--active" : ""}`}
            onClick={handleTogglePlanMode}
            title={planMode ? "退出计划模式" : "进入计划模式"}
          >
            计划模式
          </button>
        </div>
      </div>

      {/* Progress bar */}
      <div className="plan-panel__progress-bar">
        <div
          className="plan-panel__progress-fill"
          style={{ width: `${progressPct}%` }}
        />
      </div>

      {/* Status hints */}
      {inProgress > 0 && (
        <div className="plan-panel__active-hint">
          <LoaderIcon size="sm" /> {inProgress} 项进行中
        </div>
      )}

      {/* Approval buttons (shown when plan is awaiting user confirmation) */}
      {awaitingApproval && approvalActions}

      {/* Completion banner */}
      {allDone && (
        <div className="plan-panel__done-banner">
          <CheckIcon size="sm" /> 所有任务已完成
        </div>
      )}

      {/* Task list */}
      <ul className="plan-panel__list">
        {plan.entries.map((entry, idx) => (
          <PlanRow
            key={idx}
            entry={entry}
            index={idx}
            total={plan.entries.length}
            elapsed={elapsed[idx]}
            editing={editingIdx === idx}
            editText={editText}
            onEditTextChange={setEditText}
            onStartEdit={() => {
              setEditingIdx(idx);
              setEditText(entry.content);
            }}
            onSaveEdit={() => handleSaveEdit(idx)}
            onCancelEdit={() => setEditingIdx(null)}
            onSkip={() => handleSkip(idx)}
            onDelete={() => handleDeleteEntry(idx)}
            onCyclePriority={() => handleCyclePriority(idx)}
            onMoveUp={() => handleMove(idx, -1)}
            onMoveDown={() => handleMove(idx, 1)}
            onCycleStatus={() => handleCycleStatus(idx)}
          />
        ))}
      </ul>

      {/* 新增步骤(对齐 EchoAgent plan-editor) */}
      <div className="plan-panel__add">
        <input
          className="plan-panel__add-input"
          value={newStep}
          onChange={(e) => setNewStep(e.target.value)}
          onKeyDown={(e) => {
            if (e.nativeEvent.isComposing || e.nativeEvent.keyCode === 229) return;
            if (e.key === "Enter") {
              e.preventDefault();
              handleAddStep();
            }
          }}
          placeholder="新增一个步骤…"
          aria-label="新增步骤"
        />
        <button
          className="plan-panel__add-btn"
          onClick={handleAddStep}
          disabled={!newStep.trim()}
          title="新增步骤"
        >
          <AddIcon size="sm" /> 添加
        </button>
      </div>
    </div>
  );
}

interface PlanRowProps {
  entry: PlanEntry;
  index: number;
  total: number;
  elapsed?: number;
  editing: boolean;
  editText: string;
  onEditTextChange: (v: string) => void;
  onStartEdit: () => void;
  onSaveEdit: () => void;
  onCancelEdit: () => void;
  onSkip: () => void;
  onDelete: () => void;
  onCyclePriority: () => void;
  onMoveUp: () => void;
  onMoveDown: () => void;
  onCycleStatus: () => void;
}

function PlanRow({
  entry,
  index,
  total,
  elapsed,
  editing,
  editText,
  onEditTextChange,
  onStartEdit,
  onSaveEdit,
  onCancelEdit,
  onSkip,
  onDelete,
  onCyclePriority,
  onMoveUp,
  onMoveDown,
  onCycleStatus,
}: PlanRowProps) {
  const formatElapsed = (s: number) => {
    if (s < 60) return `${s}s`;
    const m = Math.floor(s / 60);
    return `${m}m ${s % 60}s`;
  };

  return (
    <li
      className={`plan-panel__row plan-panel__row--${entry.status} plan-panel__row--prio-${entry.priority}`}
    >
      <div className="plan-panel__row-icon">
        {entry.status === "completed" ? (
          <CheckIcon size="sm" />
        ) : entry.status === "in_progress" ? (
          <LoaderIcon size="sm" />
        ) : (
          <ClockIcon size="sm" />
        )}
      </div>
      <div className="plan-panel__row-body">
        {editing ? (
          <div className="plan-panel__row-edit">
            <input
              className="plan-panel__row-edit-input"
              value={editText}
              onChange={(e) => onEditTextChange(e.target.value)}
              onKeyDown={(e) => {
                if (e.nativeEvent.isComposing || e.nativeEvent.keyCode === 229) return;
                if (e.key === "Enter") onSaveEdit();
                if (e.key === "Escape") onCancelEdit();
              }}
              autoFocus
            />
            <button className="plan-panel__row-edit-save" onClick={onSaveEdit}>
              保存
            </button>
          </div>
        ) : (
          <div
            className="plan-panel__row-content"
            onDoubleClick={onStartEdit}
            title="双击编辑"
          >
            {entry.content}
          </div>
        )}
        <div className="plan-panel__row-meta">
          <button
            className="plan-panel__row-priority plan-panel__row-priority--clickable"
            onClick={onCyclePriority}
            title="点击切换优先级"
          >
            {PRIORITY_LABEL[entry.priority]}
          </button>
          <button
            type="button"
            className="plan-panel__row-status plan-panel__row-status--clickable"
            onClick={onCycleStatus}
            title="点击切换状态"
          >
            {STATUS_LABEL[entry.status]}
          </button>
          {elapsed !== undefined && (
            <span className="plan-panel__row-elapsed">
              ⏱ {formatElapsed(elapsed)}
            </span>
          )}
          <span className="plan-panel__row-index">#{index + 1}</span>
        </div>
      </div>
      {/* Row actions */}
      <div className="plan-panel__row-actions">
        <button
          className="plan-panel__row-action"
          onClick={onMoveUp}
          disabled={index === 0}
          title="上移"
          aria-label="上移"
        >
          <ArrowUpIcon size="sm" />
        </button>
        <button
          className="plan-panel__row-action"
          onClick={onMoveDown}
          disabled={index === total - 1}
          title="下移"
          aria-label="下移"
        >
          <ChevronDownIcon size="sm" />
        </button>
        {entry.status === "pending" && (
          <button
            className="plan-panel__row-action"
            onClick={onSkip}
            title="跳过此任务"
          >
            跳过
          </button>
        )}
        <button
          className="plan-panel__row-action plan-panel__row-action--danger"
          onClick={onDelete}
          title="删除此任务"
        >
          <DeleteIcon size="sm" />
        </button>
      </div>
    </li>
  );
}
