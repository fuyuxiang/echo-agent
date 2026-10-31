import type { Automation, AutomationRunRecord } from "@/lib/types";
import { formatDateInputValue, scheduledAtIso } from "./schedule-utils";

export type TaskFilter = "all" | "active" | "paused" | "ended";
export function taskState(
  task: Automation,
  records: AutomationRunRecord[] = [],
  now = new Date(),
): Exclude<TaskFilter, "all"> {
  if (task.scheduleType === "once") {
    const at = scheduledAtIso(task);
    if (
      at &&
      Date.parse(at) <= now.getTime() &&
      (task.status === "PAUSED" ||
        records.some((run) => run.automationId === task.id && run.scheduledFor))
    )
      return "ended";
  } else if (
    task.validUntilDate &&
    task.validUntilDate < formatDateInputValue(now)
  )
    return "ended";
  return task.status === "ACTIVE" ? "active" : "paused";
}
export function taskStateLabel(
  task: Automation,
  records: AutomationRunRecord[] = [],
): string {
  const state = taskState(task, records);
  if (state !== "ended") return state === "active" ? "已启用" : "已暂停";
  if (
    task.scheduleType === "once" &&
    !records.some((run) => run.automationId === task.id && run.scheduledFor)
  )
    return "已错过";
  return task.scheduleType === "once" ? "计划已结束" : "已过有效期";
}
export function runBusy(run: AutomationRunRecord): boolean {
  return run.status === "queued" || run.status === "running";
}
export function runStatusLabel(run: AutomationRunRecord): string {
  if (run.completionPending) return "已结束 · 记录保存中";
  if (run.stopRequested && runBusy(run)) return "正在停止";
  if (run.waitingReason && runBusy(run)) return run.waitingReason;
  if (run.needsAttention && runBusy(run)) return "待确认";
  return (
    (
      {
        queued: "排队中",
        running: "运行中",
        success: "已完成",
        failed: "失败",
        interrupted: "已人工接管",
        stopped: "已停止",
      } as Record<string, string>
    )[run.status] ?? run.status
  );
}
export function orderedRuns(
  records: AutomationRunRecord[],
): AutomationRunRecord[] {
  return [...records].sort(
    (a, b) => Date.parse(b.startedAt) - Date.parse(a.startedAt),
  );
}
export function nextRunLabel(value?: string, now = new Date()): string {
  if (!value) return "暂无后续执行";
  const date = new Date(value);
  if (!Number.isFinite(date.getTime())) return "执行时间待确认";
  const today = formatDateInputValue(now),
    tomorrow = new Date(now);
  tomorrow.setDate(now.getDate() + 1);
  const day = formatDateInputValue(date);
  const prefix =
    day === today
      ? "今天"
      : day === formatDateInputValue(tomorrow)
        ? "明天"
        : `${date.getFullYear() !== now.getFullYear() ? date.getFullYear() + "/" : ""}${date.getMonth() + 1}/${date.getDate()}`;
  return `${prefix} ${date.toLocaleTimeString("zh-CN", { hour: "2-digit", minute: "2-digit", hour12: false })}${date <= now ? " · 等待调度" : ""}`;
}
export function localTimeZoneLabel(): string {
  const zone = Intl.DateTimeFormat().resolvedOptions().timeZone;
  return zone === "Asia/Shanghai"
    ? "北京时间（Asia/Shanghai）"
    : `本机时区（${zone}）`;
}
export function workspaceName(cwds: string): string {
  const path = cwds
    .split(",")
    .map((x) => x.trim())
    .find(Boolean);
  return path
    ? path.split(/[\\/]/).filter(Boolean).pop() || path
    : "默认工作目录";
}
export function sessionModeLabel(task: Automation): string {
  return task.newSessionEachRun === false ? "沿用任务专属会话" : "每次新会话";
}
