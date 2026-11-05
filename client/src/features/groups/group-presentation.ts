import type { Group, GroupMessage, GroupRequest } from "./group-client";
import { terminalWork, groupWorkAutoBlocked } from "./group-client";

export function resultOnlyWorkIds(group: Group): Set<string> {
  const works = new Map(group.works.map(work => [work.id, work]));
  const ids = new Set<string>();
  for (const work of group.works) {
    if (!work.summary && !work.resultRepair) continue;
    let current: typeof work | undefined = work;
    while (current && !ids.has(current.id)) {
      ids.add(current.id);
      current = current.continuationOf ? works.get(current.continuationOf) : undefined;
    }
  }
  return ids;
}

/** Public content and operational activity have different lifecycles. */
export function publicGroupMessages(group: Group): GroupMessage[] {
  const works = new Map(group.works.map(work => [work.id, work]));
  const continued = new Set(group.works.map(work => work.continuationOf).filter(Boolean));
  const messages = group.messages.map(message => {
    const output = works.get(message.workId ?? "")?.output;
    return message.kind === "assistant" && !continued.has(message.workId) && !message.text.trim() && output?.trim()
      ? { ...message, text: output } : message;
  });
  const replied = new Set(messages.filter(message => message.kind === "assistant").map(message => message.workId));
  for (const work of group.works) {
    if (continued.has(work.id) || !work.output.trim() || replied.has(work.id)) continue;
    messages.push({ id: `result-${work.id}`, seq: group.messages[group.messages.length - 1]?.seq ?? 0,
      memberId: work.memberId, kind: "assistant", text: work.output, createdAt: work.createdAt,
      workId: work.id, replyTo: null, attachments: [], toolCalls: [] });
  }
  return messages.filter(message => ["user", "assistant", "reference"].includes(message.kind)
    && (!!message.text.trim() || message.kind === "user" && message.attachments.length > 0));
}

export function groupPublicSeq(group: Group): number {
  return group.publicSeq ?? group.messages.reduce((seq, message) => Math.max(seq, message.publicRevision ?? 0,
    ["user", "assistant"].includes(message.kind) && (message.text.trim() || message.attachments.length) ? message.seq : 0), 0);
}

/** Compatibility for older snapshots and the isolated UI fixtures. */
export function groupRequests(group: Group): GroupRequest[] {
  if (group.requests) return group.requests;
  const messages = publicGroupMessages(group);
  return [...new Set(group.works.map(work => work.rootId))].map(id => {
    const works = group.works.filter(work => work.rootId === id);
    const initial = works.filter(work => !work.parentId && !work.continuationOf && !work.summary);
    const ownerId = group.messages.find(message => message.id === id)?.requestOwnerId ?? (initial.length === 1 ? initial[0].memberId : initial.length === 0 ? works[0].memberId : works.find(work => work.summary)?.memberId ?? group.coordinatorId);
    const work = [...works].reverse().find(work => work.memberId === ownerId) ?? works[0];
    const final = work.memberId === ownerId && work.status === "completed"
      ? [...messages].reverse().find(message => message.workId === work.id && message.memberId === ownerId) : undefined;
    const state = works.some(work => work.status === "uncertain") ? "needs_check"
      : works.some(work => work.status === "stopping") ? "stopping"
      : works.some(work => !terminalWork(work.status) && (work.summary || work.resultRepair)) ? "summarizing"
      : works.some(work => ["running", "dispatching"].includes(work.status)) ? "working"
      : works.some(work => work.status === "waiting_children") ? "waiting"
      : works.some(work => work.status === "queued") ? "queued"
      : works.some(work => work.memberId === ownerId && work.status === "cancelled") ? "stopped"
      : final ? works.some(work => ["failed","cancelled"].includes(work.status) || groupWorkAutoBlocked(group, work.id)) ? "partial" : "delivered"
      : works.some(work => work.status === "failed") ? "failed" : "missing_result";
    return { id, ownerId, workId: work.id, inputRevision: Math.max(0, ...group.messages.filter(message => message.id === id || message.requestId === id).map(message => message.seq)), state, finalMessageId: ["delivered", "partial"].includes(state) ? final?.id ?? null : null };
  });
}

export const requestLabels: Record<GroupRequest["state"], string> = {
  queued: "等待处理", working: "协作中", waiting: "等待成员结果", summarizing: "正在整理结论",
  delivered: "已回复", partial: "已回复，仍有未完成事项", failed: "未能交付结果",
  missing_result: "缺少可读结果", stopped: "已停止", stopping: "正在停止", needs_check: "需要检查",
};

export const activeRequest = (request: GroupRequest) => ["queued", "working", "waiting", "summarizing", "stopping"].includes(request.state);

export function messageRequestId(group: Group, message: GroupMessage): string | null {
  return message.requestId ?? group.works.find(work => work.id === message.workId)?.rootId
    ?? (group.works.some(work => work.rootId === message.id) ? message.id : null);
}

const requestTitles = new WeakMap<GroupMessage[], Map<string, string>>();
export function requestTitle(group: Group, id: string): string {
  let titles = requestTitles.get(group.messages);
  if (!titles) {
    titles = new Map(group.messages.filter(message => message.kind === "user")
      .map(message => [message.id, message.text.trim().replace(/\s+/g, " ").slice(0, 80)]));
    requestTitles.set(group.messages, titles);
  }
  return titles.get(id) || "协作任务";
}

/** Receipts reflect native context preparation/consumption, never a model claim. */
export function followupReceipt(group: Group, message: GroupMessage): string | null {
  if (!message.requestId) return null;
  const works = group.works.filter(work => work.rootId === message.requestId);
  const owner = groupRequests(group).find(request => request.id === message.requestId)?.ownerId;
  const direct = message.memberIds?.length && !message.memberIds.includes(owner ?? "") ? message.memberIds : null;
  const relevant = works.filter(work => !direct || direct.includes(work.memberId));
  const consumed = relevant.some(work => work.consumedFollowups?.includes(message.id)
    || (work.contextSeq ?? 0) >= message.seq && ["running", "waiting_children", "completed"].includes(work.status));
  if (consumed) return "已纳入处理";
  if (relevant.some(work => work.status === "uncertain")) return "补充已保存，等待核对执行状态";
  if (works.length && works.every(work => terminalWork(work.status))) return "补充已保存在本次任务记录中";
  if (group.paused) return "补充已保存，恢复派发后处理";
  return "补充已保存，等待处理";
}

export function groupRequestTarget(group: Group, requests: GroupRequest[], selected: string | null, newTopic: boolean, replyTo: string | null, discussion?: boolean): string | null {
  const modes = new Map(group.works.map(work => [work.rootId,work.discussion]));
  const open = requests.filter(request => ["queued", "working", "waiting", "summarizing"].includes(request.state)
    && (discussion === undefined || modes.get(request.id) === discussion));
  const available = (id: string | null | undefined) => id && open.some(request => request.id === id) ? id : null;
  if (newTopic) return null;
  if (replyTo) {
    const message = group.messages.find(message => message.id === replyTo);
    return available(message?.requestId ?? group.works.find(work => work.id === message?.workId)?.rootId
      ?? (requests.some(request => request.id === replyTo) ? replyTo : null));
  }
  if (selected) return available(selected);
  return open.length === 1 ? open[0].id : null;
}

export function groupActivity(group: Group): string {
  if (group.works.some(work => work.status === "uncertain")) return "";
  const works = group.works.filter(work => !terminalWork(work.status));
  if (!works.length) return "";
  if (group.paused) return "已暂停后续派发，正在执行的工作会继续完成";
  const running = works.filter(work => ["running", "dispatching"].includes(work.status));
  return (running.length ? running : works).slice(0, 2).map(work => {
    const name = group.members.find(member => member.id === work.memberId)?.name ?? "成员";
    const tool = group.messages.some(message => message.workId === work.id && message.toolCalls.some(call => call.status === "in_progress" || call.status === "pending"));
    const action = work.summary || work.resultRepair ? "正在整理结果"
      : work.status === "waiting_children" ? "正在等待成员结果" : work.status === "queued" ? "正在排队"
      : work.status === "stopping" ? "正在停止" : tool ? "正在处理任务" : work.output.trim() ? "正在回复" : "正在思考";
    return `${name}${action}…`;
  }).join("；");
}
