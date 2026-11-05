import { invoke } from "@tauri-apps/api/core";
import { isQuotaBlocking, loadQuotaConfig, loadUsage } from "@/lib/usage-quota";
import { listen } from "@tauri-apps/api/event";
import type { PermissionMode, PermissionModeStatus, PermissionModeSetResult } from "@/lib/agent-client";
export { groupDiscussionOnly } from "./group-intent";

export interface GroupMember { removed?: boolean; id: string; name: string; expert: string | null; role: string; modelId: string; sessionId: string | null; discussionSessionId: string | null; generation: number }
export interface GroupKnowledge { personal: boolean; organization: boolean; organizationScopeIds: string[]; context: string }
export interface GroupMessage { sourceAttachmentPaths?: string[] | null; publicRevision?: number; requestOwnerId?: string | null; sourceRequestId?: string | null; assignedWorkIds?: string[]; memberIds?: string[]; requestId?: string | null; knowledge?: GroupKnowledge; id: string; seq: number; memberId: string | null; kind: string; text: string; createdAt: number; workId: string | null; replyTo: string | null; attachments: string[]; toolCalls: Record<string, unknown>[] }
export interface GroupRequest { id: string; ownerId: string; workId: string; inputRevision: number; state: "queued" | "working" | "waiting" | "summarizing" | "delivered" | "partial" | "failed" | "missing_result" | "stopped" | "stopping" | "needs_check"; finalMessageId: string | null }
export interface GroupWork { contextSeq?: number; consumedFollowups?: string[]; summary?: boolean; resultRepair?: boolean; id: string; rootId: string; parentId: string | null; continuationOf: string | null; memberId: string; prompt: string; status: string; discussion: boolean; sessionId: string | null; output: string; error: string | null; createdAt: number; tokens: number }
export interface GroupDecision { id: string; text: string; messageId: string | null; confirmed: boolean; voided?: boolean; history?: { text: string; changedAt: number; action?: string }[]; updatedAt?: number | null }
export interface Group { permissionMode?: PermissionMode; latestRequestId?: string | null; publicSeq?: number; requests?: GroupRequest[]; id: string; name: string; goal: string; cwd: string; sourceSessionId: string | null; sourceTeamId: string | null; coordinatorId: string; members: GroupMember[]; messages: GroupMessage[]; works: GroupWork[]; decisions: GroupDecision[]; archived: boolean; pinned: boolean; paused: boolean; revision: number; updatedAt: number }
export interface CreateGroupInput { requestId?: string; coordinatorName?: string; coordinatorRole?: string; name: string; goal: string; cwd: string; modelId: string; members: string[]; sourceSessionId?: string; sourceTeamId?: string; referenceText?: string; roles?: Record<string, string>; models?: Record<string, string> }
export interface SendGroupInput { sourceAttachmentPaths?: string[]; sourceRequestId?: string | null; newRequest?: boolean; requestId?: string | null; knowledge?: GroupKnowledge; id: string; text: string; memberIds: string[]; discussion: boolean; attachments: string[]; replyTo: string | null }
export const groupList = () => invoke<Group[]>("group_list");
export const groupSnapshot = (groupId: string) => invoke<Group>("group_snapshot", { groupId });
export const groupOwner = (sessionId: string) => invoke<string | null>("group_owner", { sessionId });
export const groupPermissionModeGet = (groupId: string) => invoke<PermissionModeStatus>("group_permission_mode_get", { groupId });
export const groupPermissionModeSet = (groupId: string, mode: PermissionMode) => invoke<PermissionModeSetResult>("group_permission_mode_set", { groupId, mode });
export async function resolveGroupForSession(sessionId: string, groups: Group[]): Promise<string | null> {
  const owner = groups.find(group => group.members.some(member => member.sessionId === sessionId || member.discussionSessionId === sessionId)
    || group.works.some(work => work.sessionId === sessionId));
  // The ordinary catalog includes hidden sessions, while group list previews
  // intentionally bound their work history. Always resolve a cache miss.
  return owner?.id ?? await groupOwner(sessionId) ?? null;
}
export const groupCreate = (input: CreateGroupInput) => invoke<Group>("group_create", { input });
export const isGroupStopRequest = (input: SendGroupInput) => !input.attachments.length && ["停止", "停止全部", "停止任务", "停止当前任务", "取消任务", "stop"].includes(input.text.trim().replace(/[。！!]+$/, "").toLowerCase());
export async function groupSend(groupId: string, input: SendGroupInput): Promise<void> {
  if (!isGroupStopRequest(input) && isQuotaBlocking(loadUsage(), loadQuotaConfig())) throw new Error("已达到 Token 配额，请先在“用量统计”中调整上限或策略");
  await invoke<void>("group_send", { groupId, input });
}
export const groupControl = (groupId: string, action: "pause" | "resume" | "stop" | "archive" | "pin" | "retry" | "summarize" | "stop_request", workId?: string) => invoke<void>("group_control", { groupId, action, workId: workId ?? null });
export const groupMetadata = (groupId: string, patch: { name?: string; pinned?: boolean; archived?: boolean }) => invoke<void>("group_metadata", { groupId, name: patch.name ?? null, pinned: patch.pinned ?? null, archived: patch.archived ?? null });
export const groupDelete = (groupId: string, stopWork: boolean) => invoke<{ sessionIds: string[] }>("group_delete", { groupId, stopWork });
export interface GroupCleanupReceipt { groupId: string; sessionIds: string[] }
export const groupCleanupPending = async () => await invoke<GroupCleanupReceipt[]>("group_cleanup_pending");
export const groupCleanupAck = async (groupId: string) => { await invoke<void>("group_cleanup_ack", { groupId }); };
export const groupSettings = (groupId: string, input: { expectedRevision?: number; name: string; goal: string; coordinatorId: string; cwd?: string; members: { id: string; role: string; modelId: string }[] }) => invoke<void>("group_settings", { groupId, input });
export const onGroupChanged = (callback: (groupId: string, deleted?: boolean, sessionIds?: string[]) => void) => listen<{ groupId: string; deleted?: boolean; sessionIds?: string[] }>("group://changed", event => callback(event.payload.groupId, event.payload.deleted, event.payload.sessionIds));

export const terminalWork = (status: string) => ["completed", "failed", "cancelled"].includes(status);
export function hasAllMemberMention(text: string): boolean {
  return /(?:^|\s)@(?:全体|all)(?:$|[\s，,。；;：:！!？?])/i.test(text.replace(/```[\s\S]*?```|`[^`]*`/g, " "));
}
export function mentionedMembers(text: string, members: GroupMember[]): string[] {
  const body = text.replace(/```[\s\S]*?```|`[^`]*`/g, " ");
  if (hasAllMemberMention(text)) return members.filter(member => member.expert !== null && !member.removed).map(member => member.id);
  const spans: { start: number; end: number }[] = [], found: string[] = [];
  for (const member of members.filter(member => !member.removed).sort((a, b) => b.name.length - a.name.length)) {
    const escaped = member.name.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
    const regex = new RegExp(`(?:^|\\s)@${escaped}(?=$|[\\s，,。；;：:！!？?])`, "g");
    for (const match of body.matchAll(regex)) {
      const start = match.index! + match[0].indexOf("@");
      const end = start + member.name.length + 1;
      if (spans.some(span => start < span.end && end > span.start)) continue;
      spans.push({ start, end }); found.push(member.id);
    }
  }
  return [...new Set(found)];
}
export function groupRecipients(group: Group, text: string, explicit: string[], replyTo: string | null): string[] {
  const active = group.members.filter(member => !member.removed);
  const selected = [...new Set(explicit.filter(id => active.some(member => member.id === id)))];
  if (selected.length) return selected;
  const mentioned = mentionedMembers(text, active);
  if (mentioned.length) return mentioned;
  const author = group.messages.find(message => message.id === replyTo)?.memberId;
  if (author && active.some(member => member.id === author)) return [author];
  return [group.coordinatorId];
}
export const workLabels: Record<string, string> = { queued: "待执行", dispatching: "正在启动", running: "执行中", waiting_children: "等待成员结果", completed: "已完成", failed: "执行失败", stopping: "正在停止", cancelled: "已停止", uncertain: "需要检查" };
/** Supplements retain their message id but report lifecycle against their task. */
export function latestGroupRequestId(group: Group): string | undefined {
  if (group.latestRequestId) return group.latestRequestId;
  const user = [...group.messages].reverse().find(message => message.kind === "user");
  return user ? user.requestId ?? user.id : group.works.find(work => work.id === group.messages[group.messages.length - 1]?.workId)?.rootId;
}
export function failedGroupWorks(group: Group): GroupWork[] {
  const request = latestGroupRequestId(group);
  return group.works.filter(work => work.rootId === request && work.status === "failed"
    && !group.works.some(next => next.continuationOf === work.id));
}
/** Historical silent Auto denials can finish a turn without finishing its work. */
const autoBlockedWorkCache = new WeakMap<GroupMessage[], Set<string>>();
export function groupWorkAutoBlocked(group: Group, workId: string): boolean {
  // Native snapshots replace their message array. Index once per snapshot so
  // opening a long work history does not rescan every message for each row.
  let ids = autoBlockedWorkCache.get(group.messages);
  if (!ids) {
    ids = new Set(group.messages.filter(message => message.workId && message.toolCalls.some(call => call.status === "failed"
      && JSON.stringify([call.content, call.rawOutput]).includes("Auto mode blocked this action"))).map(message => message.workId!));
    autoBlockedWorkCache.set(group.messages, ids);
  }
  return ids.has(workId);
}
export function autoBlockedGroupWorks(group: Group): GroupWork[] {
  const root = latestGroupRequestId(group);
  return group.works.filter(work => work.rootId === root && work.status === "completed" && !work.summary && !work.resultRepair
    && groupWorkAutoBlocked(group, work.id));
}
export function groupStatus(group: Group): string {
  if (group.archived) return "已归档";
  if (group.works.some(work => work.status === "uncertain")) return "需要检查";
  if (group.works.some(work => work.status === "stopping")) return "正在停止";
  if (group.paused) return "已暂停";
  const active = group.works.filter(work => ["running", "dispatching", "stopping"].includes(work.status)).length;
  if (active) return `${active} 人工作中`;
  if (group.works.some(work => !terminalWork(work.status))) return "等待协作";
  const delivery = group.requests?.find(request => request.id === latestGroupRequestId(group));
  if (delivery?.state === "failed") return "执行失败";
  if (delivery?.state === "stopped") return "已停止";
  if (delivery?.state === "missing_result") return "待整理结果";
  if (delivery?.state === "partial") return "有未完成事项";
  if (delivery?.state === "delivered") return "已回复";
  if (failedGroupWorks(group).length) return "执行失败";
  return "当前空闲";
}

export interface TeamTemplate { coordinatorName?: string | null; coordinatorRole?: string | null; defaultModelId?: string | null; id: string; name: string; description: string; source: string; members: { name: string; role: string; modelId?: string }[] }
export const groupTemplates = () => invoke<TeamTemplate[]>("group_team_templates");
export const groupSaveTeam = (groupId: string, name: string) => invoke<TeamTemplate>("group_team_save", { groupId, name });
export const groupDeleteTeam = (templateId: string) => invoke<void>("group_team_delete", { templateId });
export const groupMemberChange = (groupId: string, action: "add" | "remove" | "refresh" | "clear_memory", memberId?: string, expert?: string) => invoke<void>("group_member_change", { groupId, action, memberId: memberId ?? null, expert: expert ?? null });
export const groupDecision = (groupId: string, text: string, messageId: string, decisionId?: string) => invoke<void>("group_decision", { groupId, text, messageId, decisionId: decisionId ?? null });

export const groupDecisionChange = (groupId: string, decisionId: string, action: "edit" | "void" | "restore", text?: string) => invoke<void>("group_decision_change", { groupId, decisionId, action, text: text ?? null });


/** Effective attachment inheritance, including an earlier round's exclusions. */
export function groupSourceAttachments(group: Group, root: string): string[] {
  const paths = new Set<string>(), seen = new Set<string>();
  let cursor: string | null | undefined = root;
  while (cursor && !seen.has(cursor)) {
    seen.add(cursor);
    for (const message of group.messages) if (message.id === cursor || message.requestId === cursor)
      for (const path of message.attachments) paths.add(path);
    const message = group.messages.find(message => message.id === cursor);
    if (message?.sourceAttachmentPaths) {
      for (const path of message.sourceAttachmentPaths) paths.add(path);
      break;
    }
    cursor = message?.sourceRequestId;
  }
  return [...paths];
}
