import { useEffect, useMemo, useState } from "react";
import { PageToolbar } from "../PageToolbar";
import {
  ArrowLeft,
  Play,
  Pencil,
  Square,
  RotateCcw,
  ChevronDown,
  Clock3,
  AlertCircle,
  CheckCircle2,
} from "lucide-react";
import type { Automation, AutomationRunRecord } from "@/lib/types";
import { automationRunResult } from "@/lib/agent-client";
import { Markdown } from "@/components/Markdown";
import { modelDisplayName } from "@/lib/model-presentation";
import { createMarkdownHostConfig } from "@/lib/markdown-host";
import {
  describeSchedule,
  describeValidity,
  formatRunTime,
} from "./schedule-utils";
import {
  localTimeZoneLabel,
  nextRunLabel,
  orderedRuns,
  runBusy,
  runStatusLabel,
  sessionModeLabel,
  taskStateLabel,
  taskState,
  workspaceName,
} from "./task-presentation";

export function AutomationTaskDetail({
  task,
  records,
  starting,
  initialRecordId,
  onBack,
  onEdit,
  onRun,
  onPause,
  onReset,
  onStop,
  onDelete,
  onArchive,
  onOpenSession,
  onSettings,
  onToast,
}: {
  task?: Automation;
  records: AutomationRunRecord[];
  starting: boolean;
  initialRecordId?: string;
  onBack: () => void;
  onEdit: (task: Automation) => void;
  onRun: (id: string) => void;
  onPause: (task: Automation) => void;
  onReset: (task: Automation) => void;
  onStop: (id: string) => Promise<void>;
  onDelete: (id: string) => void;
  onArchive: (id: string) => void;
  onOpenSession?: (sessionId: string, cwd?: string) => void;
  onSettings?: () => void;
  onToast?: (message: string) => void;
}) {
  const runs = orderedRuns(records);
  const latestId = runs[0]?.id;
  const [selectedId, setSelectedId] = useState(initialRecordId ?? latestId);
  const [result, setResult] = useState<{
    text: string;
    truncated: boolean;
  } | null>(null);
  const [resultError, setResultError] = useState("");
  const [loading, setLoading] = useState(false);
  const [reload, setReload] = useState(0);
  const [stopping, setStopping] = useState(false);
  const selected = runs.find((run) => run.id === selectedId) ?? runs[0];
  const markdownConfig = useMemo(() => {
    const { onApplyCode: _apply, ...reading } = createMarkdownHostConfig({
      cwd: selected?.cwd,
      sessionId: selected?.sessionId,
      onToast,
    });
    return reading;
  }, [selected?.cwd, selected?.sessionId, onToast]);
  const busy = starting || runs.some(runBusy);
  useEffect(() => {
    setSelectedId(initialRecordId ?? latestId);
  }, [initialRecordId, latestId]);
  useEffect(() => {
    let active = true;
    setResult(null);
    setResultError("");
    if (!selected?.sessionId) {
      setLoading(false);
      return;
    }
    setLoading(true);
    void automationRunResult(selected.id)
      .then((value) => {
        if (active) setResult(value);
      })
      .catch((error) => {
        if (active) setResultError(String(error).replace(/^Error:\s*/, ""));
      })
      .finally(() => {
        if (active) setLoading(false);
      });
    return () => {
      active = false;
    };
  }, [selected?.id, selected?.status, selected?.finishedAt, reload]);
  const name = task?.name ?? runs[0]?.automationName ?? "定时任务";
  return (
    <div className="atm-task-detail">
      <PageToolbar className="atm-task-detail__toolbar">
        <button type="button" className="atm-toolbar-btn" onClick={onBack}>
          <ArrowLeft size={16} />
          任务列表
        </button>
        {task && (
          <div className="atm-task-detail__actions">
            <button
              type="button"
              className="atm-create-btn"
              disabled={busy}
              onClick={() => onRun(task.id)}
            >
              <Play size={16} />
              {busy ? "任务执行中" : "立即运行"}
            </button>
            <button
              type="button"
              className="atm-toolbar-btn"
              onClick={() => onEdit(task)}
            >
              <Pencil size={16} />
              编辑任务
            </button>
            {taskState(task, runs) !== "ended" && (
              <button
                type="button"
                className="atm-toolbar-btn"
                onClick={() => onPause(task)}
              >
                {task.status === "ACTIVE" ? "暂停计划" : "启用计划"}
              </button>
            )}
          </div>
        )}
      </PageToolbar>
      <h1 className="atm-task-detail__title">{name}</h1>
      {task ? (
        <>
          <div className="atm-task-detail__meta">
            <span>{taskStateLabel(task, runs)}</span>
            <span>{describeSchedule(task)}</span>
            {task.status === "ACTIVE" && (
              <span>下次：{nextRunLabel(task.nextRunAt)}</span>
            )}
          </div>
          <p className="atm-task-detail__muted">
            {localTimeZoneLabel()} · 本机运行 ·{" "}
            <span title={task.cwds}>{workspaceName(task.cwds)}</span> ·{" "}
            {task.permissionMode === "fullAccess" ? "完全访问" : "默认权限"}
          </p>
          <div className="atm-task-detail__meta">
            <span>会话方式：{sessionModeLabel(task)}</span>
            {task.newSessionEachRun === false && (
              <button
                type="button"
                className="atm-text-button"
                onClick={() => onReset(task)}
              >
                <RotateCcw size={14} />
                重置会话上下文
              </button>
            )}
          </div>
          {task.newSessionEachRun === false && (
            <p className="atm-task-detail__muted">
              在专属会话中补充的内容会影响后续运行。每次结果仍单独保留。
            </p>
          )}
          <details className="atm-task-config">
            <summary>
              任务要求与执行配置
              <ChevronDown size={14} />
            </summary>
            <p className="atm-task-config__prompt">{task.prompt}</p>
            <dl>
              <dt>工作目录</dt>
              <dd>{task.cwds || "默认工作目录"}</dd>
              <dt>通知</dt>
              <dd>
                {task.pushToWeChat
                  ? "按通知设置提醒；结果单独记录"
                  : "仅保留运行记录"}
              </dd>
              <dt>有效期</dt>
              <dd>{describeValidity(task) || "长期有效"}</dd>
              <dt>技能与连接器</dt>
              <dd>
                {[...task.skills, ...task.connectorIds, task.expertName]
                  .filter(Boolean)
                  .join("、") || "未额外配置"}
              </dd>
            </dl>
          </details>
        </>
      ) : (
        <p className="atm-task-detail__muted">
          {runs.length
            ? "原任务已删除，历史运行结果仍保留。"
            : "未找到关联任务或运行记录，可能已被删除或清理。"}
        </p>
      )}
      <section
        className="atm-run-result"
        aria-label="本次运行结果"
        aria-live="polite"
      >
        <h2>
          {selected && selected.id !== latestId
            ? "历史运行结果"
            : "最近运行结果"}
        </h2>
        {!selected ? (
          <div className="atm-detail-empty">
            <Clock3 size={22} />
            <p>还没有运行记录</p>
            <span>
              {task
                ? "任务会按计划执行，也可以立即运行一次查看结果。"
                : "返回任务列表查看其他任务。"}
            </span>
          </div>
        ) : (
          <>
            <div
              className={`atm-run-result__state atm-run-result__state--${selected.needsAttention ? "attention" : selected.status}`}
            >
              {selected.status === "success" ? (
                <CheckCircle2 size={18} />
              ) : (
                <AlertCircle size={18} />
              )}
              <strong>{runStatusLabel(selected)}</strong>
              <span>
                {formatRunTime(selected.startedAt)} ·{" "}
                {selected.scheduledFor ? "定时触发" : "手动运行"}
              </span>
            </div>
            {selected.scheduledFor && (
              <p className="atm-task-detail__muted">
                计划时间：
                {new Date(selected.scheduledFor).toLocaleString("zh-CN")}
              </p>
            )}
            {selected.error && (
              <p className="atm-run-result__error">{selected.error}</p>
            )}
            {selected.needsAttention && (
              <p>本次运行需要你确认权限或补充信息，处理后才能继续。</p>
            )}
            {loading ? (
              <p className="atm-task-detail__muted" role="status">
                正在读取本次结果…
              </p>
            ) : resultError ? (
              <div role="alert">
                <p>{resultError}</p>
                <button
                  type="button"
                  className="atm-toolbar-btn"
                  onClick={() => setReload((value) => value + 1)}
                >
                  重新读取结果
                </button>
              </div>
            ) : result?.text ? (
              <div className="atm-run-result__content">
                <Markdown config={markdownConfig} complete={!runBusy(selected)}>
                  {result.text}
                </Markdown>
              </div>
            ) : (
              <p className="atm-task-detail__muted">
                {runBusy(selected)
                  ? "结果生成中，可进入会话查看实时过程。"
                  : selected.status === "success"
                    ? "本次运行未留下可直接展示的文本结果，可查看完整会话。"
                    : "本次没有可展示的结果，可查看错误说明或完整会话。"}
              </p>
            )}
            {result?.truncated && (
              <p className="atm-task-detail__muted">
                这里展示可读取的部分结果。完整内容请进入会话查看。
              </p>
            )}
            {selected.notificationDelivery && (
              <div className="atm-notification-receipt">
                <span>通知：{selected.notificationDelivery}</span>
                {/未.*成功|未知|失败/.test(selected.notificationDelivery) &&
                  onSettings && (
                    <button
                      type="button"
                      className="atm-text-button"
                      onClick={onSettings}
                    >
                      检查通知设置
                    </button>
                  )}
              </div>
            )}
            <details className="atm-run-audit">
              <summary>本次执行信息</summary>
              <dl>
                <dt>开始时间</dt>
                <dd>{new Date(selected.startedAt).toLocaleString("zh-CN")}</dd>
                {selected.finishedAt && (
                  <>
                    <dt>结束时间</dt>
                    <dd>
                      {new Date(selected.finishedAt).toLocaleString("zh-CN")}
                    </dd>
                  </>
                )}
                {selected.modelId && (
                  <>
                    <dt>实际模型</dt>
                    <dd>{modelDisplayName(selected.modelId)}</dd>
                  </>
                )}
                {selected.cwd && (
                  <>
                    <dt>工作目录</dt>
                    <dd>{selected.cwd}</dd>
                  </>
                )}
                {selected.executionConfig && (
                  <>
                    <dt>会话方式</dt>
                    <dd>
                      {selected.executionConfig.newSessionEachRun
                        ? "每次新会话"
                        : "沿用任务专属会话"}
                    </dd>
                    <dt>执行权限</dt>
                    <dd>
                      {selected.executionConfig.permissionMode === "fullAccess"
                        ? "完全访问"
                        : "默认权限"}
                    </dd>
                    <dt>当次任务要求</dt>
                    <dd className="atm-task-config__prompt">
                      {selected.executionConfig.prompt}
                    </dd>
                  </>
                )}
              </dl>
            </details>
            <div className="atm-task-detail__actions">
              {selected.sessionId && onOpenSession && (
                <button
                  type="button"
                  className="atm-toolbar-btn"
                  onClick={() =>
                    onOpenSession(selected.sessionId!, selected.cwd)
                  }
                >
                  {selected.needsAttention ? "前往处理" : "查看完整会话"}
                </button>
              )}
              {runBusy(selected) && !selected.completionPending && (
                <button
                  type="button"
                  className="atm-toolbar-btn"
                  disabled={stopping}
                  title={selected.stopRequested ? "停止尚未确认，可再次发送停止请求；后续计划不受影响" : undefined}
                  onClick={async () => {
                    setStopping(true);
                    try {
                      await onStop(selected.id);
                    } finally {
                      setStopping(false);
                    }
                  }}
                >
                  <Square size={14} />
                  {stopping ? "正在请求停止…" : selected.stopRequested ? "再次请求停止" : "停止本次运行"}
                </button>
              )}
              {task && selected.status === "failed" && (
                <button
                  type="button"
                  className="atm-toolbar-btn"
                  disabled={busy}
                  onClick={() => onRun(task.id)}
                >
                  使用当前配置重试
                </button>
              )}
              {!runBusy(selected) && !selected.archived && (
                <button
                  type="button"
                  className="atm-text-button"
                  onClick={() => onArchive(selected.id)}
                >
                  归档记录
                </button>
              )}
            </div>
          </>
        )}
      </section>
      <section className="atm-task-history" aria-label="运行历史">
        <h2>
          运行历史 <span>{runs.length}</span>
        </h2>
        {runs.map((run) => (
          <button
            type="button"
            className={`atm-task-history__row${run.id === selected?.id ? " is-selected" : ""}`}
            key={run.id}
            aria-pressed={run.id === selected?.id}
            onClick={() => setSelectedId(run.id)}
          >
            <span>
              {formatRunTime(run.startedAt)} ·{" "}
              {run.scheduledFor ? "定时触发" : "手动运行"}
              {run.archived ? " · 已归档" : ""}
            </span>
            <span>{runStatusLabel(run)}</span>
          </button>
        ))}
      </section>
      {task && (
        <div className="atm-task-detail__danger">
          <button
            type="button"
            className="atm-text-button"
            disabled={busy}
            onClick={() => onDelete(task.id)}
          >
            删除任务
          </button>
          <span>取消后续计划，保留历史结果。</span>
        </div>
      )}
    </div>
  );
}
