import type { ProjectMeta } from "@/stores/projects-store";
import type { SessionSummary } from "./types";
import type { UnifiedTask } from "./project-tasks";
import type { TeamProjectSnapshot } from "./team-project-client";

export type ProjectTaskState = "pending" | "running" | "attention" | "review" | "completed" | "paused" | "blocked" | "unknown" | "syncing";
export const PROJECT_TASK_STATES: Record<ProjectTaskState, { label: string; hint: string }> = {
  pending: { label: "待开始", hint: "可交给 Agent 开始执行" },
  running: { label: "进行中", hint: "Agent 正在推进任务" },
  attention: { label: "需要你处理", hint: "打开任务处理授权、问题或计划确认" },
  review: { label: "待验收", hint: "本轮执行已结束，请检查结果后验收" },
  completed: { label: "已验收", hint: "已确认交付结果" },
  paused: { label: "已暂停", hint: "打开原任务继续，已有上下文保留" },
  blocked: { label: "有阻塞", hint: "打开任务检查错误或补充要求" },
  unknown: { label: "状态待检查", hint: "打开原任务核对历史进度" },
  syncing: { label: "待同步", hint: "恢复项目连接后核对共享状态" },
};

/** Execution has priority over an old acceptance flag; a later turn reopens review. */
export function projectTaskState(row: UnifiedTask, summary?: SessionSummary): ProjectTaskState {
  if (!row.private && !row.remote) return "syncing";
  if (row.remote) {
    if (row.remote.status === "in_progress" && summary?.status?.startsWith("awaiting_")) return "attention";
    return ({ pending: "pending", in_progress: "running", blocked: "blocked", review: "review", accepted: "completed" } as const)[row.remote.status];
  }
  if (summary?.status && ["working", "planning", "pausing", "stopping"].includes(summary.status)) return "running";
  if (summary?.status?.startsWith("awaiting_")) return "attention";
  if (summary?.status === "failed") return "blocked";
  if (summary?.status === "paused" || summary?.status === "stopped") return "paused";
  if (row.local?.status === "completed" && (!row.local.acceptedAt || !summary?.updatedAt || Date.parse(summary.updatedAt) <= Date.parse(row.local.acceptedAt))) return "completed";
  if (summary?.status === "completed") return "review";
  if (row.sessionId && !summary?.status) return "unknown";
  if (row.local?.status === "in_progress") return "running";
  if (row.local?.status === "paused") return "paused";
  return "pending";
}

export interface ProjectFeedEntry { id: string; content: string; actor: string; time: string; kind: string; taskId?: string }
const EVENT_NAMES: Record<string, string> = {
  "project.created": "创建了项目", "project.updated": "更新了项目设置",
  "member.added": "邀请了项目成员", "member.removed": "移除了项目成员", "member.updated": "调整了成员权限",
  "task.created": "创建了任务", "task.updated": "更新了任务", "task.claimed": "接手了任务",
  "run.started": "开始了任务执行", "run.completed": "结束了本轮执行", "run.failed": "任务执行失败", "run.cancelled": "停止了任务执行",
  "decision.created": "记录了项目决定", "decision.updated": "更新了项目决定", "asset.added": "添加了共享资料", "asset.updated": "更新了共享资料",
};
export function projectFeed(project: ProjectMeta, snapshot: TeamProjectSnapshot | null, summaries: SessionSummary[]): ProjectFeedEntry[] {
  const entries: ProjectFeedEntry[] = (project.activity ?? []).map(e => ({ id: e.id, content: e.content, actor: "我", time: e.createdAt, kind: e.kind, taskId: e.kind === "task" ? e.resourceId : undefined }));
  for (const comment of snapshot?.comments ?? []) entries.push({ id: `comment:${comment.id}`, content: comment.content, actor: comment.actorName, time: new Date(comment.createdAt).toISOString(), kind: "comment" });
  for (const event of snapshot?.events ?? []) {
    if (event.kind === "comment_created") continue;
    const run = snapshot?.runs?.find(r => r.id === event.resourceId);
    const task = snapshot?.tasks.find(t => t.id === (run?.taskId ?? event.resourceId));
    const kind = event.kind.replaceAll("_", ".");
    const label = EVENT_NAMES[kind] ?? ({ "run.updated": "更新了执行记录", "member.role.updated": "调整了成员权限", "decision.proposed": "提出了项目决定", "asset.published": "发布了共享资料" } as Record<string, string>)[kind] ?? "更新了项目内容";
    entries.push({ id: `remote:${event.id}`, content: `${label}${task ? `「${task.title}」` : ""}`, actor: event.actorName, time: new Date(event.createdAt).toISOString(), kind: event.kind, taskId: task ? `shared:${task.id}` : undefined });
  }
  for (const task of project.tasks) {
    const summary = summaries.find(s => s.sessionId === task.sessionId);
    if (!task.sharedTasks && summary?.updatedAt) entries.push({ id: `session:${task.sessionId}`, content: `更新了任务「${task.title}」`, actor: "我", time: summary.updatedAt, kind: "task", taskId: task.id });
  }
  if (!entries.length) entries.push({ id: "created", content: "创建了项目", actor: "我", time: project.createdAt, kind: "settings" });
  return entries.sort((a, b) => Date.parse(b.time) - Date.parse(a.time));
}
