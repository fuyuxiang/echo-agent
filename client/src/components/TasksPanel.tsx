/**
 * 运行中任务面板 - 显示 EchoAgent 的后台任务和子代理
 *
 * EchoAgent 通过工具调用启动的 background tasks 和 spawn_subagent 在后台运行。
 * 这里通过 `echo.agent/task/list` + `echo.agent/subagent/list_running` 观测它们，
 * 通过 `echo.agent/task/kill` / `echo.agent/subagent/cancel` 取消。
 *
 * 这个面板嵌入到 ChatView 右侧或作为浮层。监听 `agent://task-update` 自动刷新。
 */
import { useCallback, useEffect, useLayoutEffect, useRef, useState, type CSSProperties } from "react";
import {
  TaskListIcon,
  DeleteIcon,
  RefreshCwIcon,
  ChevronDownIcon,
  CloseIcon,
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
}

export function TasksPanel({ sessionId, refreshSignal, onToast }: TasksPanelProps) {
  const [tasks, setTasks] = useState<RunningTask[]>([]);
  const [tasksSessionId, setTasksSessionId] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [collapsed, setCollapsed] = useState(false);
  const [dismissed, setDismissed] = useState(false);
  const [killingTaskKeys, setKillingTaskKeys] = useState<Set<string>>(() => new Set());
  const killingTaskKeysRef = useRef(new Set<string>());
  const sessionEpochRef = useRef(0);
  const currentSessionRef = useRef(sessionId);
  currentSessionRef.current = sessionId;
  const [dockStyle, setDockStyle] = useState<CSSProperties>();
  const reloadGenerationRef = useRef(0);
  const lastRefreshSignalRef = useRef(refreshSignal);
  const onToastRef = useRef(onToast);

  useLayoutEffect(() => {
    if (!sessionId || tasksSessionId !== sessionId || dismissed || tasks.length === 0) return;
    let composer: HTMLElement | null = null;
    let resizeObserver: ResizeObserver | null = null;
    const place = () => {
      if (!composer) return;
      const rect = composer.getBoundingClientRect();
      const nextStyle = {
        bottom: Math.max(16, window.innerHeight - rect.top + 12),
        right: Math.max(16, window.innerWidth - rect.right),
        width: Math.min(320, Math.max(220, rect.width - 24)),
        maxHeight: Math.max(96, Math.min(480, rect.top - 112)),
      };
      setDockStyle((current) => current
        && current.bottom === nextStyle.bottom && current.right === nextStyle.right
        && current.width === nextStyle.width && current.maxHeight === nextStyle.maxHeight
        ? current : nextStyle);
    };
    const watchComposer = () => {
      const next = document.querySelector<HTMLElement>(".chatview .echo-composer");
      if (next === composer) return;
      resizeObserver?.disconnect();
      composer = next;
      if (composer && typeof ResizeObserver !== "undefined") {
        resizeObserver = new ResizeObserver(place);
        resizeObserver.observe(composer);
        // A centered, max-width composer can move without changing size when
        // the sidebar or tool pane opens. Follow its layout containers too.
        for (let parent = composer.parentElement; parent && parent !== document.body; parent = parent.parentElement) {
          resizeObserver.observe(parent);
        }
      }
      if (composer) place();
      else setDockStyle(undefined);
    };
    const observer = new MutationObserver(watchComposer);
    observer.observe(document.body, { childList: true, subtree: true });
    watchComposer();
    window.addEventListener("resize", place);
    return () => {
      observer.disconnect();
      resizeObserver?.disconnect();
      window.removeEventListener("resize", place);
    };
  }, [dismissed, sessionId, tasks.length, tasksSessionId]);

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
    setDismissed(false);
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
    // A new lifecycle event re-opens a panel the user previously dismissed,
    // but preserves their compact/expanded preference.
    setDismissed(false);
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
        onToast?.("已终止任务");
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

  if (!sessionId || tasksSessionId !== sessionId || dismissed || tasks.length === 0) {
    return null;
  }

  return (
    <aside
      className={`tasks-panel${collapsed ? " tasks-panel--collapsed" : ""}`}
      aria-label="运行中任务"
      style={dockStyle}
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
          <button
            className="tasks-panel__control"
            onClick={() => setDismissed(true)}
            title="关闭；任务状态变化时将再次显示"
            aria-label="关闭运行中任务"
          >
            <CloseIcon size="sm" />
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
                      {task.description ?? task.id}
                    </div>
                    <div className="tasks-panel__item-meta">
                      {task.kind && <span>{task.kind}</span>}
                      {task.status && <span>· {task.status}</span>}
                      <span className="tasks-panel__item-id">#{task.id.slice(0, 8)}</span>
                    </div>
                  </div>
                  <button
                    className="tasks-panel__kill"
                    onClick={() => handleKill(task)}
                    disabled={killing}
                    title={killing ? "正在终止" : "终止"}
                    aria-label={`终止任务：${task.description ?? task.id}`}
                  >
                    <DeleteIcon size="sm" />
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
