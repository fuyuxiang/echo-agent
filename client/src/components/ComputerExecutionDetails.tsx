import type { AutomationStatus } from "@/lib/automation-client";

/** Read-only task context inside the existing execution disclosure. Tool
 * records own chronological actions/images; this never polls or controls UI. */
export function ComputerExecutionDetails({ status }: { status: AutomationStatus }) {
  const task = status.task;
  const target = status.selectedTarget?.name || status.activeWindow?.application || status.browserTitle;
  if (!task?.scope) return null;
  return <div className="execution-process__computer-context" aria-label="应用操作情况" data-phase={task.phase}>
    {target && <p>当前应用：{target}</p>}
    {task.outcome && <p>{task.outcome}</p>}
    <p className="execution-process__hint">{status.execution?.backgroundInput
      ? "支持的操作按应用定向执行；需要真实桌面输入时会使用前台。"
      : "原生控件优先直接操作；鼠标键盘后备需要使用前台。"}</p>
    {status.execution?.inputConflictDetection === false &&
      <p className="execution-process__hint">系统输入监测尚不可用，原生写入不会执行；请检查系统授权。</p>}
    {(task.phase === "stopped" || task.phase === "paused") &&
      <p className="execution-process__hint">可以直接发送消息继续或调整要求，后续会重新读取当前界面。</p>}
  </div>;
}
