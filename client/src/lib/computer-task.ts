import type { AutomationStatus } from "./automation-client";

export function computerTaskRunning(status: AutomationStatus | null | undefined): boolean {
  return Boolean(status?.task?.authorized && !status.paused &&
    ["running", "verifying", "waiting_user", "result_unknown"].includes(status.task.phase));
}
export function computerTaskLabel(status: AutomationStatus): string {
  const target = status.selectedTarget?.name || status.activeWindow?.application || status.browserTitle;
  switch (status.task?.phase) {
    case "running": return target ? `正在操作${target}` : "正在处理应用中的任务";
    case "verifying": return "正在核验操作结果";
    case "waiting_user": return status.task.reason === "message_text_required" ? "等待消息内容" : status.task.authorized ? "等待你确认操作" : "等待你继续";
    case "completed": return "操作结果已核验";
    case "stopped": return "任务已停止";
    case "paused": return "操作已中断，可在对话中说明下一步";
    case "failed": return "任务尚未完成";
    default: return "操作结果尚未确认";
  }
}
