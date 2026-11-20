/**
 * 会话回溯 — 从会话菜单打开回溯时间线。
 *
 * 回溯能力：
 *  - Rewind（回溯）：调 `echo.agent/rewind/{points,execute}`，回到指定 prompt 索引。
 *    支持 Runtime 规范 mode: conversation_only（仅回退对话）/
 *    files_only（仅文件）/ all（对话和可核验文件）。
 *
 * 增强点（对齐 EchoAgent）：
 *  - 时间线视图：每个回溯点显示时间、prompt 预览、assistant 回复预览、工具调用徽章。
 *  - 文件/记忆变更徽章：标记哪些步骤产生了文件改动或记忆写入。
 *  - 三种模式按钮：仅对话 / 仅文件 / 全量。
 */
import { createPortal } from "react-dom";
import { useCallback, useEffect, useId, useLayoutEffect, useRef, useState } from "react";
import {
  rewindExecute,
  rewindPoints,
  rewindPreview,
  rewindUndoExecute,
  rewindUndoPreview,
} from "@/lib/agent-client";
import type { RewindFileConflict, RewindMode, RewindPoint, RewindPreview } from "@/lib/types";
import { friendlyError } from "@/lib/error-format";
import {
  ClockIcon,
  ChevronDownIcon,
} from "@/foundation/components/Icon/icons";
import { useModalFocus } from "@/lib/use-modal-focus";
import { useAppDialog } from "./AppDialog";

const MODE_LABELS: Record<RewindMode, string> = {
  conversation_only: "仅对话",
  files_only: "仅文件",
  all: "全量",
};

const MODE_TITLES: Record<RewindMode, string> = {
  conversation_only: "回退对话历史，不影响文件",
  files_only: "回退文件改动，不影响对话",
  all: "回退对话和可核验的文件改动",
};

const CONFLICT_LABELS: Record<string, string> = {
  modified_externally: "文件内容已变化",
  created_externally: "已有后来创建的文件",
  deleted_externally: "文件已被删除",
  unverified_history: "历史版本无法核验",
  unavailable: "无法安全读取文件",
  restore_failed: "文件恢复失败",
  rewind_interrupted: "上次写入中断，结果待核对",
  concurrent_copy_preserved: "回溯期间的修改已另存",
};

function FileConflicts({ conflicts }: { conflicts: RewindFileConflict[] }) {
  return <ul>{conflicts.map(({ path, conflictType, recoveryPath }) => (
    <li key={path}>
      <code>{path}</code> · {CONFLICT_LABELS[conflictType] ?? "无法核验"}
      {recoveryPath && <div>安全副本 <code>{recoveryPath}</code></div>}
    </li>
  ))}</ul>;
}

function FilePreview({ preview }: { preview: RewindPreview }) {
  return <div className="rewind-bar__file-preview">
    <p>{preview.cleanFiles.length ? `可恢复 ${preview.cleanFiles.length} 个文件` : "没有可恢复的文件。"}</p>
    {preview.cleanFiles.length > 0 && <ul>{preview.cleanFiles.map(path => <li key={path}><code>{path}</code></li>)}</ul>}
    {preview.conflicts.length > 0 && <>
      <p>以下 {preview.conflicts.length} 个文件存在冲突，将保留当前状态。</p>
      <FileConflicts conflicts={preview.conflicts} />
    </>}
  </div>;
}

interface RewindBarProps {
  /** Recheck live session state immediately before a history mutation. */
  canExecute?: () => boolean;
  minPromptIndex?: number;
  action?: "rewind";
  onActionDone?: () => void;
  sessionId: string;
  onRewound?: (sessionId: string) => void | Promise<void>;
  onToast?: (msg: string) => void;
}

export function RewindBar({
  sessionId,
  onRewound,
  onToast,
  action,
  onActionDone,
  canExecute,
  minPromptIndex,
}: RewindBarProps) {
  const [open, setOpen] = useState(action === "rewind");
  const [points, setPoints] = useState<RewindPoint[]>([]);
  const [pointsLoaded, setPointsLoaded] = useState(false);
  const [loading, setLoading] = useState(false);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [executionConflicts, setExecutionConflicts] = useState<RewindFileConflict[]>([]);
  const [busy, setBusy] = useState(false);
  /** Currently selected mode for the next rewind action. */
  const [selectedMode, setSelectedMode] = useState<RewindMode>("all");
  const activeSessionRef = useRef(sessionId);
  const requestGenerationRef = useRef(0);
  const dialogId = useId();
  const dialogTitleId = useId();
  const closeTimeline = () => { setOpen(false); onActionDone?.(); };
  const executionGuardRef = useRef(canExecute);
  executionGuardRef.current = canExecute;
  const canStartAction = () => {
    if (executionGuardRef.current?.() !== false) return true;
    onToast?.("任务状态已变化，请等待任务停止后重试");
    closeTimeline();
    return false;
  };
  const dialogRef = useModalFocus<HTMLDivElement>(open, () => { if (!busy) closeTimeline(); });
  const { requestConfirmation, dialog } = useAppDialog(sessionId);

  const loadPoints = useCallback(async () => {
    const targetSessionId = sessionId;
    const generation = ++requestGenerationRef.current;
    setLoading(true);
    setLoadError(null);
    try {
      const next = await rewindPoints(targetSessionId);
      if (
        activeSessionRef.current === targetSessionId
        && requestGenerationRef.current === generation
      ) {
        setPoints(next);
        setPointsLoaded(true);
      }
    } catch (error) {
      if (
        activeSessionRef.current === targetSessionId
        && requestGenerationRef.current === generation
      ) {
        setPoints([]);
        setLoadError(friendlyError(error));
      }
    } finally {
      if (
        activeSessionRef.current === targetSessionId
        && requestGenerationRef.current === generation
      ) {
        setLoading(false);
      }
    }
  }, [sessionId]);

  useLayoutEffect(() => {
    activeSessionRef.current = sessionId;
    requestGenerationRef.current += 1;
    setOpen(action === "rewind");
    setPoints([]);
    setPointsLoaded(false);
    setLoadError(null);
    setExecutionConflicts([]);
    setLoading(false);
    setBusy(false);
    setSelectedMode("all");
  }, [sessionId]);

  useEffect(() => {
    if (open && !pointsLoaded && !loading && !loadError) void loadPoints();
  }, [open, pointsLoaded, loading, loadError, loadPoints]);

  const handleRewind = async (idx: number, targetMode: RewindMode, preview?: RewindPreview) => {
    if (!canStartAction()) return;
    if (minPromptIndex !== undefined && idx < minPromptIndex) {
      onToast?.("继承的历史仅供查看，请返回原对话或从该位置接续新对话");
      return;
    }
    const targetSessionId = sessionId;
    setBusy(true);
    setExecutionConflicts([]);
    try {
      const result = preview
        ? await rewindExecute(targetSessionId, idx, targetMode, true, preview.revision)
        : await rewindExecute(targetSessionId, idx, targetMode, true);
      const label = MODE_LABELS[targetMode];
      const skipped = result.conflicts?.length ?? 0;
      if (activeSessionRef.current === targetSessionId) setExecutionConflicts(result.conflicts ?? []);
      onToast?.(skipped
        ? targetMode === "files_only" && !result.revertedFiles?.length
          ? `${skipped} 个文件需要核对`
          : `已回溯（${label}），${skipped} 个文件需要核对`
        : `已回溯（${label}）`);
      if (targetMode !== "files_only" || !skipped || result.revertedFiles?.length) await onRewound?.(targetSessionId);
      if (activeSessionRef.current === targetSessionId) {
        if (skipped) await loadPoints();
        else closeTimeline();
      }
    } catch (e) {
      onToast?.(`回溯失败：${friendlyError(e)}`);
    } finally {
      if (activeSessionRef.current === targetSessionId) setBusy(false);
    }
  };

  const confirmRewind = async (point: RewindPoint) => {
    const mode = selectedMode;
    const generation = requestGenerationRef.current;
    let preview: RewindPreview | undefined;
    if (mode !== "conversation_only") {
      setBusy(true);
      try {
        preview = await rewindPreview(sessionId, point.promptIndex, mode);
      } catch (error) {
        if (requestGenerationRef.current === generation) onToast?.(`无法预览回溯：${friendlyError(error)}`);
        return;
      } finally {
        if (requestGenerationRef.current === generation) setBusy(false);
      }
      if (activeSessionRef.current !== sessionId || requestGenerationRef.current !== generation) return;
    }
    const noFilesToRestore = mode === "files_only" && preview?.cleanFiles.length === 0;
    requestConfirmation({
      title: `确认回溯（${MODE_LABELS[mode]}）？`,
      description: <>
        <p>回到第 {point.promptIndex + 1} 步执行前{point.promptPreview ? `：${point.promptPreview}` : ""}。</p>
        <p>{mode === "all" ? "这会撤销此步骤及后续的对话，并恢复预览中可恢复的文件。" : mode === "files_only" ? "这会恢复预览中可恢复的文件，保留对话。" : "这会撤销此步骤及后续的对话，文件保持原状。"}</p>
        {preview && <FilePreview preview={preview} />}
      </>,
      danger: !noFilesToRestore,
      confirmLabel: noFilesToRestore ? "知道了" : "确认回溯",
      action: () => noFilesToRestore ? undefined : handleRewind(point.promptIndex, mode, preview),
    });
  };

  const confirmUndo = async () => {
    if (!canStartAction()) return;
    const target = sessionId;
    setBusy(true);
    try {
      const preview = await rewindUndoPreview(target);
      if (activeSessionRef.current !== target) return;
      if (!preview) {
        onToast?.("没有可撤销的文件恢复记录");
        return;
      }
      requestConfirmation({
        title: "撤销上次文件恢复？",
        description: <FilePreview preview={preview} />,
        danger: preview.cleanFiles.length > 0,
        confirmLabel: preview.cleanFiles.length ? "确认撤销" : "知道了",
        action: async () => {
          if (!preview.cleanFiles.length || !canStartAction()) return;
          const result = await rewindUndoExecute(target, preview.revision);
          const restored = result.revertedFiles?.length ?? 0;
          const conflicts = result.conflicts?.length ?? 0;
          if (activeSessionRef.current === target) setExecutionConflicts(result.conflicts ?? []);
          onToast?.(conflicts ? `已撤销 ${restored} 个文件的恢复，${conflicts} 个文件需要核对` : `已撤销 ${restored} 个文件的恢复`);
          if (restored) await onRewound?.(target);
        },
      });
    } catch (error) {
      if (activeSessionRef.current === target) onToast?.(`无法预览撤销：${friendlyError(error)}`);
    } finally {
      if (activeSessionRef.current === target) setBusy(false);
    }
  };

  return (
    <div className={action ? "session-history-actions" : "rewind-bar"}>
      {!action && <button
        type="button"
        className="rewind-bar__btn"
        onClick={() => setOpen((v) => !v)}
        disabled={busy}
        title="回溯到历史某一步"
        aria-haspopup="dialog"
        aria-expanded={open}
        aria-controls={dialogId}
      >
        <ClockIcon size="sm" /> 回溯
        <ChevronDownIcon size="sm" />
      </button>}

      {open && createPortal(
        <div className="workbench-dialog-overlay" onMouseDown={event => { if (event.currentTarget === event.target && !busy) closeTimeline(); }}>
        <div
          ref={dialogRef}
          id={dialogId}
          className="rewind-bar__dropdown rewind-bar__dropdown--timeline"
          role="dialog"
          aria-modal="true"
          aria-labelledby={dialogTitleId}
          tabIndex={-1}
        >
          {/* Header with refresh */}
          <div className="rewind-bar__header">
            <span id={dialogTitleId}>回溯时间线</span>
            <button type="button" className="main-topbar__btn" aria-label="关闭回溯时间线" disabled={busy} onClick={closeTimeline}>×</button>
            <button
              type="button"
              className="rewind-bar__refresh"
              onClick={loadPoints}
              disabled={loading || busy}
              aria-label="刷新回溯点"
            >
              {loading ? "加载中…" : "刷新"}
            </button>
          </div>

          {/* Mode selector */}
          <div className="rewind-bar__modes">
            {(Object.keys(MODE_LABELS) as RewindMode[]).map((mode) => (
              <button
                type="button"
                key={mode}
                className={
                  "rewind-bar__mode-btn" +
                  (selectedMode === mode ? " rewind-bar__mode-btn--active" : "")
                }
                onClick={() => setSelectedMode(mode)}
                disabled={busy}
                title={MODE_TITLES[mode]}
                aria-pressed={selectedMode === mode}
                data-modal-initial-focus={selectedMode === mode ? "" : undefined}
              >
                {MODE_LABELS[mode]}
              </button>
            ))}
          </div>
          <button type="button" className="rewind-bar__refresh" disabled={busy || loading} onClick={() => void confirmUndo()}>
            撤销上次文件恢复
          </button>
          {executionConflicts.length > 0 && <div className="rewind-bar__file-preview" role="status">
            <p>{executionConflicts.length} 个文件需要核对</p>
            <FileConflicts conflicts={executionConflicts} />
          </div>}

          {/* Timeline list */}
          {loading && <div className="rewind-bar__empty">加载中…</div>}
          {!loading && loadError && (
            <div className="rewind-bar__empty" role="alert">
              <div>加载回溯点失败：{loadError}</div>
              <button type="button" onClick={() => void loadPoints()}>重试</button>
            </div>
          )}
          {!loading && !loadError && points.length === 0 && (
            <div className="rewind-bar__empty">还没有回溯点，走几步再回头看。</div>
          )}
          <ul className="rewind-bar__timeline">
            {points.map((p) => (
              <li key={p.promptIndex} className="rewind-bar__timeline-item">
                {/* Timeline dot + connector line */}
                <div className="rewind-bar__timeline-rail">
                  <span className="rewind-bar__timeline-dot" />
                </div>

                {/* Content card */}
                <div className="rewind-bar__timeline-card">
                  <div className="rewind-bar__timeline-time">
                    第 {p.promptIndex + 1} 步{p.timestamp ? ` · ${new Date(p.timestamp).toLocaleString()}` : ""}
                  </div>
                  {p.promptPreview && (
                    <div className="rewind-bar__timeline-prompt">
                      {p.promptPreview.length > 80
                        ? p.promptPreview.slice(0, 80) + "…"
                        : p.promptPreview}
                    </div>
                  )}
                  {p.messagePreview && (
                    <div className="rewind-bar__timeline-response">
                      💬{" "}
                      {p.messagePreview.length > 60
                        ? p.messagePreview.slice(0, 60) + "…"
                        : p.messagePreview}
                    </div>
                  )}

                  {/* Badges: file changes / memory changes / tool names */}
                  <div className="rewind-bar__timeline-badges">
                    {p.hasFileChanges && (
                      <span className="rewind-bar__badge rewind-bar__badge--file">
                        📄 文件
                      </span>
                    )}
                    {p.hasMemoryChanges && (
                      <span className="rewind-bar__badge rewind-bar__badge--memory">
                        🧠 记忆
                      </span>
                    )}
                    {p.toolNames && p.toolNames.length > 0 && (
                      <span className="rewind-bar__badge rewind-bar__badge--tool">
                        🔧 {p.toolNames.slice(0, 3).join(", ")}
                        {p.toolNames.length > 3 && ` +${p.toolNames.length - 3}`}
                      </span>
                    )}
                  </div>

                  {/* Rewind action button */}
                  <button
                    type="button"
                    className="rewind-bar__timeline-action"
                    onClick={() => void confirmRewind(p)}
                    disabled={busy || minPromptIndex !== undefined && p.promptIndex < minPromptIndex}
                    title={minPromptIndex !== undefined && p.promptIndex < minPromptIndex ? "继承的历史仅供查看，请返回原对话回溯，或从该位置接续新对话" : undefined}
                  >
                    {minPromptIndex !== undefined && p.promptIndex < minPromptIndex ? "继承的历史（只读）" : `回溯到此处（${MODE_LABELS[selectedMode]}）`}
                  </button>
                </div>
              </li>
            ))}
          </ul>
        </div>
        </div>, document.body
      )}
      {dialog}
    </div>
  );
}
