import type { SessionSummary } from "./types";
import type { Group } from "@/features/groups/group-client";

export type TrayTaskState = "waiting" | "running" | "recent";
export interface TrayTarget { kind: "session" | "group"; id: string }
export interface TrayTask extends TrayTarget { title: string; state: TrayTaskState }
export interface TraySnapshot { waitingCount: number; runningCount: number; tasks: TrayTask[] }
export type TrayAction = { id: string; kind: "new" | "all" | "session" | "group"; targetId?: string };

const waiting = new Set(["awaiting_permission", "awaiting_answer", "awaiting_approval"]);
const running = new Set(["working", "planning", "pausing", "stopping"]);
const runningGroupWork = new Set(["running", "dispatching", "stopping"]);
export const trayTargetKey = (target: TrayTarget) => `${target.kind}:${target.id}`;
const timestamp = (value?: string) => {
  const parsed = value ? Date.parse(value) : 0;
  return Number.isFinite(parsed) ? parsed : 0;
};

/** One entry per user-visible task, independently of workspace/sidebar filters. */
export function buildTraySnapshot(
  sessions: SessionSummary[],
  groups: Group[],
  waitingSessionIds: ReadonlySet<string>,
  visits: Readonly<Record<string, number>>,
): TraySnapshot {
  const internal = new Set(groups.flatMap(group => [
    ...group.members.flatMap(member => [member.sessionId, member.discussionSessionId]),
    ...group.works.map(work => work.sessionId),
  ]).filter((id): id is string => !!id));
  const candidates = new Map<string, TrayTask & { recent: number; updated: number }>();
  const waitingIds = new Set([...waitingSessionIds, ...sessions.filter(session => waiting.has(session.status ?? "")).map(session => session.sessionId)]);
  const add = (task: TrayTask, updated: number) => {
    const key = trayTargetKey(task);
    candidates.set(key, { ...task, updated, recent: visits[key] ?? updated });
  };
  for (const session of sessions) {
    if (!session.sessionId || session.archived || session.hidden || session.sessionKind === "subagent" || internal.has(session.sessionId)) continue;
    const state = waitingIds.has(session.sessionId)
      ? "waiting" : running.has(session.status ?? "") ? "running" : "recent";
    add({ kind: "session", id: session.sessionId, title: session.title || "未命名任务", state }, timestamp(session.updatedAt));
  }
  for (const group of groups) {
    if (group.archived) continue;
    const members = [...group.members.flatMap(member => [member.sessionId, member.discussionSessionId]), ...group.works.map(work => work.sessionId)];
    const state = members.some(id => !!id && waitingIds.has(id)) ? "waiting"
      : group.works.some(work => runningGroupWork.has(work.status)) ? "running" : "recent";
    add({ kind: "group", id: group.id, title: group.name || "未命名专家团", state }, group.updatedAt);
  }
  const tasks = [...candidates.values()];
  const priority = { waiting: 0, running: 1, recent: 2 };
  tasks.sort((a, b) => priority[a.state] - priority[b.state]
    || b.recent - a.recent || b.updated - a.updated || trayTargetKey(a).localeCompare(trayTargetKey(b)));
  return {
    waitingCount: tasks.filter(task => task.state === "waiting").length,
    runningCount: tasks.filter(task => task.state === "running").length,
    tasks: tasks.slice(0, 3).map(({ recent: _recent, updated: _updated, ...task }) => task),
  };
}

/** Keep only a small navigation history, never message content or filesystem paths. */
export function recordTrayVisit(visits: Readonly<Record<string, number>>, target: TrayTarget, now = Date.now()): Record<string, number> {
  return Object.fromEntries(Object.entries({ ...visits, [trayTargetKey(target)]: now })
    .sort((a, b) => b[1] - a[1]).slice(0, 100));
}

/** Serialize IPC writes; a slow older snapshot must never overwrite a newer one. */
export function createTrayPublisher(send: (snapshot: TraySnapshot) => Promise<unknown>, onError: (error: unknown) => void) {
  let pending: TraySnapshot | undefined, last = "", sending = false, disposed = false;
  const flush = async () => {
    if (sending || disposed) return;
    sending = true;
    try {
      while (pending && !disposed) {
        const snapshot = pending; pending = undefined;
        const key = JSON.stringify(snapshot);
        if (last === key) continue;
        try { await send(snapshot); last = key; }
        catch (error) { onError(error); }
      }
    } finally { sending = false; }
  };
  return {
    publish(snapshot: TraySnapshot) { if (!disposed) { pending = snapshot; void flush(); } },
    dispose() { disposed = true; pending = undefined; },
  };
}
