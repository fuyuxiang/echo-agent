import type { AutomationStatus } from "@/lib/automation-client";

/** Read-only task context inside the existing execution disclosure. Tool
 * records own chronological actions/images; this never polls or controls UI. */
export function ComputerExecutionDetails({ status }: { status: AutomationStatus }) {
  const task = status.task;
  const target = status.selectedTarget?.name || status.activeWindow?.application || status.browserTitle;
  if (!task?.scope) return null;
  return <div className="execution-process__computer-context" aria-label="应用操作情况" data-phase={task.phase}>
    <p>{target ? `当前应用：${target}` : `操作范围：${task.scope}`}</p>
    {task.outcome && <p>{task.outcome}</p>}
    {status.execution?.inputConflictDetection === false &&
      <p className="execution-process__hint">系统输入监测尚不可用，请检查系统授权后重试。</p>}
  </div>;
}
