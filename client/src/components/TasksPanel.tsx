/**
 * 运行中任务面板 - 显示 EchoAgent 的后台任务和子代理
 *
 * EchoAgent 通过工具调用启动的 background tasks 和 spawn_subagent 在后台运行。
 * 这里通过 `echo.agent/task/list` + `echo.agent/subagent/list_running` 观测它们，
 * 通过 `echo.agent/task/kill` / `echo.agent/subagent/cancel` 取消。
 *
 * 详情嵌入任务详情容器；会话页头只显示数量摘要。通过 refreshSignal 刷新。
 */
import { useCallback, useEffect, useRef, useState } from "react";
import {
  TaskListIcon,
  CloseIcon,
  RefreshCwIcon,
  ChevronDownIcon,
} from "@/foundation/components/Icon/icons";
import { taskKill, tasksList } from "@/lib/agent-client";
import type { RunningTask } from "@/lib/types";

interface TasksPanelProps {
  /** Live parent session whose work should be shown. */
  sessionId?: string;
  /** Optional: listen for task update events to auto-refresh. Set to a counter
   *  that increments on each `agent://task-update`. */
  refreshSignal?: number;
  onToast?: (msg: string) => void;
  presentation?: "summary" | "details";
  onOpen?: () => void;
}

export function TasksPanel({ sessionId, refreshSignal, onToast, presentation = "details", onOpen }: TasksPanelProps) {
  const [tasks, setTasks] = useState<RunningTask[]>([]);
  const [tasksSessionId, setTasksSessionId] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [collapsed, setCollapsed] = useState(false);
  const [killingTaskKeys, setKillingTaskKeys] = useState<Set<string>>(() => new Set());
  const killingTaskKeysRef = useRef(new Set<string>());
  const sessionEpochRef = useRef(0);
  const currentSessionRef = useRef(sessionId);
  currentSessionRef.current = sessionId;
  const reloadGenerationRef = useRef(0);
  const lastRefreshSignalRef = useRef(refreshSignal);
  const onToastRef = useRef(onToast);

  useEffect(() => {
    onToastRef.current = onToast;
  }, [onToast]);

  const reload = useCallback(async () => {
    const generation = ++reloadGenerationRef.current;
    if (!sessionId) return;
    setLoading(true);
    try {
      const nextTasks = await tasksList(sessionId);
      if (reloadGenerationRef.current !== generation) return;
      setTasks(nextTasks);
      setTasksSessionId(sessionId);
      setError(null);
    } catch (cause) {
      if (reloadGenerationRef.current !== generation) return;
      // Keep the last known tasks visible so a transient list failure never
      // removes the user's only way to terminate background work.
      const message = String(cause).replace(/^Error:\s*/, "");
      setError(message);
      onToastRef.current?.(`加载运行中任务失败：${message}`);
    } finally {
      if (reloadGenerationRef.current === generation) setLoading(false);
    }
  }, [sessionId]);

  useEffect(() => {
    // A session switch must immediately remove controls belonging to the
    // previous session. No active session means no task request at all.
    reloadGenerationRef.current += 1;
    setTasks([]);
    setTasksSessionId(null);
    setError(null);
    setCollapsed(false);
    sessionEpochRef.current += 1;
    killingTaskKeysRef.current = new Set();
    setKillingTaskKeys(new Set());
    setLoading(false);
    if (sessionId) void reload();
    return () => {
      reloadGenerationRef.current += 1;
      sessionEpochRef.current += 1;
    };
  }, [reload, sessionId]);

  useEffect(() => {
    if (lastRefreshSignalRef.current === refreshSignal) return;
    lastRefreshSignalRef.current = refreshSignal;
    if (!sessionId) return;
    // Lifecycle updates preserve the reader's expanded preference.
    void reload();
  }, [refreshSignal, reload, sessionId]);

  const handleKill = useCallback(
    async (task: RunningTask) => {
      if (!sessionId) return;
      const taskKey = `${task.source}:${task.id}`;
      if (killingTaskKeysRef.current.has(taskKey)) return;
      const epoch = sessionEpochRef.current;
      const isCurrentSession = () => currentSessionRef.current === sessionId && sessionEpochRef.current === epoch;
      killingTaskKeysRef.current.add(taskKey);
      setKillingTaskKeys(new Set(killingTaskKeysRef.current));
      try {
        await taskKill(sessionId, task.id, task.source);
        if (!isCurrentSession()) return;
        setTasks((current) => current.filter(
          (entry) => entry.id !== task.id || entry.source !== task.source,
        ));

        void reload();
      } catch (e) {
        if (isCurrentSession()) onToast?.(`终止失败：${String(e).replace(/^Error:\s*/, "")}`);
      } finally {
        if (isCurrentSession()) {
          killingTaskKeysRef.current.delete(taskKey);
          setKillingTaskKeys(new Set(killingTaskKeysRef.current));
        }
      }
    },
    [onToast, reload, sessionId],
  );

  if (!sessionId || tasksSessionId !== sessionId && !error) return null;
  if (presentation === "summary") {
    if (!tasks.length && !error) return null;
    const label = error ? "检查后台任务" : `后台任务 ${tasks.length}`;
    return <button type="button" className="session-background-tasks" aria-label={label} data-tip={label} onClick={onOpen}>
      <TaskListIcon size="sm" /><span>{error ? "检查" : tasks.length}</span>
    </button>;
  }

  return (
    <aside
      className={`tasks-panel tasks-panel--inline${collapsed ? " tasks-panel--collapsed" : ""}`}
      aria-label="运行中任务"
    >
      <div className="tasks-panel__header">
        <h3 className="tasks-panel__title">
          <TaskListIcon size="sm" /> 运行中任务 ({tasks.length})
        </h3>
        <div className="tasks-panel__actions">
          {!collapsed && (
            <button
              className="tasks-panel__control"
              onClick={reload}
              disabled={loading}
              title="刷新"
              aria-label={error ? "重试加载运行中任务" : "刷新运行中任务"}
            >
              <RefreshCwIcon size="sm" />
            </button>
          )}
          <button
            className="tasks-panel__control tasks-panel__collapse"
            onClick={() => setCollapsed((current) => !current)}
            title={collapsed ? "展开" : "收起"}
            aria-label={collapsed ? "展开运行中任务" : "收起运行中任务"}
            aria-expanded={!collapsed}
          >
            <ChevronDownIcon size="sm" />
          </button>

        </div>
      </div>
      {!collapsed && (
        <>
          {error && (
            <div className="tasks-panel__loading" role="alert">
              刷新失败：{error}。已保留上次结果。
            </div>
          )}
          {tasks.length === 0 && !error && <p className="tool-side-panel__empty">当前没有运行中的后台任务</p>}
          <ul className="tasks-panel__list">
            {tasks.map((task) => {
              const taskKey = `${task.source}:${task.id}`;
              const killing = killingTaskKeys.has(taskKey);
              return (
                <li key={taskKey} className="tasks-panel__item">
                  <div className="tasks-panel__item-icon">
                    <TaskListIcon size="sm" />
                  </div>
                  <div className="tasks-panel__item-body">
                    <div className="tasks-panel__item-desc">
                      {task.description ?? "后台任务"}
                    </div>
                    <div className="tasks-panel__item-meta">
                      <span>{({running:"运行中",working:"运行中",in_progress:"运行中",pending:"等待执行",completed:"已结束",failed:"执行失败",stopped:"已停止",paused:"已暂停"} as Record<string,string>)[task.status ?? ""] ?? "状态待确认"}</span>
                      <details><summary>诊断信息</summary><code>{task.kind} · {task.source}:{task.id}</code></details>
                    </div>
                  </div>
                  <button
                    className="tasks-panel__kill"
                    onClick={() => handleKill(task)}
                    disabled={killing}
                    title={killing ? "正在终止" : "终止"}
                    aria-label={`终止任务：${task.description ?? "后台任务"}`}
                  >
                    <CloseIcon size="sm" />
                  </button>
                </li>
              );
            })}
            {loading && <li className="tasks-panel__loading">刷新中…</li>}
          </ul>
        </>
      )}
    </aside>
  );
}
