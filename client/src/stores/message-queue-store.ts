/**
 * 消息队列 store —— 对齐 EchoAgent `session:enqueueMessage/getMessageQueue/...`。
 *
 * agent 工作时仍可继续排队多条 prompt:编辑/重排/暂停/恢复/取消/立即发送。
 * 每条队列为「按会话隔离」的有序列表;完成一轮对话后由调用方(App)取下一条
 * active(非 paused)项继续 `agentSend`,实现「回完一条自动发下一条」。
 *
 * 持久化待发送内容；重启后暂停恢复，发送结果不确定的行需用户检查记录。
 */
import { create } from "zustand";
import { readDurable, writeDurable } from "@/lib/durable-ui-state";

export type QueueItemStatus = "queued" | "paused" | "sending";
type EditableQueueItemStatus = Exclude<QueueItemStatus, "sending">;
export type SendingSettlement = "consume" | "retry";

export interface QueueTerminalPolicy {
  /** How an in-flight queue item should leave the `sending` state. */
  settlement: SendingSettlement;
  /** Whether this terminal event may advance the queue. */
  autoAdvance: boolean;
  /** Whether the conversation should be presented as failed. */
  failed: boolean;
}

export interface CodingQueuedContext { mode: "ask" | "plan" | "agent"; editorContext?: string; imagePaths?: string[] }

export interface QueueItem {
  /** 稳定 id(用于 React key 与操作寻址)。 */
  id: string;
  /** 排队的 prompt 文本。 */
  text: string;
  /** Local paths that must travel with this queued prompt. */
  attachments?: string[];
  coding?: CodingQueuedContext;
  status: QueueItemStatus;
  recovery?: "queued" | "uncertain";
  sendError?: string;
  /** Prompt correlation id while this exact row is owned by an Agent turn. */
  promptId?: string;
  /** 入队时间戳(ms)。 */
  createdAt: number;
}

/** sessionId → 有序队列。 */
type QueueMap = Record<string, QueueItem[]>;

interface QueueState {
  queues: QueueMap;
  /** 入队一条(追加到末尾,默认 queued)。返回新 item 的 id。 */
  enqueue: (sessionId: string, text: string, attachments?: string[], coding?: CodingQueuedContext) => string;
  /** 编辑某条文本。 */
  update: (sessionId: string, id: string, text: string) => boolean;
  /** 删除某条。 */
  remove: (sessionId: string, id: string) => boolean;
  /** 移动某条到新位置(0-based)。越界则 clamp。 */
  reorder: (sessionId: string, from: number, to: number) => boolean;
  /** 暂停 / 恢复。 */
  setStatus: (sessionId: string, id: string, status: EditableQueueItemStatus) => boolean;
  /** Hold waiting follow-ups after the user stops or the active turn fails. */
  pauseQueued: (sessionId: string) => void;
  /** Atomically reserve the first queued item so completion events cannot
   *  dispatch the same long-running request twice. */
  claimNext: (sessionId: string, promptId?: string) => QueueItem | null;
  /** Atomically reserve a specific row selected by the user. */
  claimById: (sessionId: string, id: string, promptId: string) => QueueItem | null;
  /** Settle the in-flight row owned by a terminal Agent event. Consumed items
   *  leave the queue; retryable failures return to `queued`. */
  settleSending: (
    sessionId: string,
    settlement: SendingSettlement,
    promptId?: string,
  ) => QueueItem | null;
  /** Preserve one uncertain dispatch for explicit review, without affecting other tasks. */
  holdSending: (sessionId: string, promptId: string, uncertain?: boolean, message?: string) => void;
  /** Return all process-owned in-flight rows to a retryable state. */
  retryAllSending: () => void;
  /** 取下一条 active(非 paused)项并从队列移除;无则返回 null。 */
  shiftNext: (sessionId: string) => QueueItem | null;
  /** 清空某会话的整个队列。 */
  clear: (sessionId: string) => void;
  /** 读取某会话的队列(只读视图)。 */
  getQueue: (sessionId: string) => QueueItem[];
}

const newId = () =>
  `q_${Date.now().toString(36)}_${Math.random().toString(36).slice(2, 8)}`;

/** 安全读写:返回队列的可变副本,缺省为空数组。 */
function queueOf(map: QueueMap, sessionId: string): QueueItem[] {
  return map[sessionId] ?? [];
}

const OUTBOX_KEY = "echoagent.outbox.v1";
function validQueues(value: unknown): value is QueueMap {
  return !!value && typeof value === "object" && !Array.isArray(value)
    && Object.values(value).every((items) => Array.isArray(items) && items.every((item) =>
      item && typeof item.id === "string" && typeof item.text === "string"
      && ["queued", "paused", "sending"].includes(item.status) && Number.isFinite(item.createdAt)
      && (item.promptId === undefined || typeof item.promptId === "string")
      && (item.attachments === undefined || (Array.isArray(item.attachments) && item.attachments.every((p: unknown) => typeof p === "string")))));
}
function restoredQueues(): QueueMap {
  const queues = readDurable<QueueMap>(OUTBOX_KEY, {}, validQueues);
  return Object.fromEntries(Object.entries(queues).map(([id, items]) => [id, items.map((item) => ({
    ...item, status: "paused" as const,
    recovery: item.recovery ?? (item.status === "sending" ? "uncertain" as const : "queued" as const),
  }))]));
}

/** User edits are acknowledged only once the restart snapshot is saved. */
function commitQueues(queues: QueueMap, set: (state: { queues: QueueMap }) => void): boolean {
  if (!writeDurable(OUTBOX_KEY, queues)) return false;
  set({ queues });
  return true;
}

export const useMessageQueueStore = create<QueueState>((set, get) => ({
  queues: restoredQueues(),
  enqueue: (sessionId, text, attachments = [], coding) => {
    const id = newId();
    const item: QueueItem = {
      id,
      text,
      attachments: [...new Set(attachments)],
      ...(coding ? {coding} : {}),
      status: "queued",
      createdAt: Date.now(),
    };
    const queues = { ...get().queues, [sessionId]: [...queueOf(get().queues, sessionId), item] };
    if (!writeDurable(OUTBOX_KEY, queues)) throw new Error("队列未能保存，请保留输入并检查本机存储空间");
    set({ queues });
    return id;
  },
  update: (sessionId, id, text) => {
    const { queues } = get();
    const q = queueOf(queues, sessionId);
    if (!q.some((it) => it.id === id)) return true;
    return commitQueues({ ...queues, [sessionId]: q.map((it) => it.id === id ? { ...it, text } : it) }, set);
  },
  remove: (sessionId, id) => {
    const q = queueOf(get().queues, sessionId);
    if (!q.some((it) => it.id === id)) return true;
    const next = q.filter((it) => it.id !== id);
    const queues = { ...get().queues };
    if (next.length === 0) delete queues[sessionId];
    else queues[sessionId] = next;
    return commitQueues(queues, set);
  },
  reorder: (sessionId, from, to) => {
    const { queues } = get();
    const q = queueOf(queues, sessionId);
    if (from < 0 || from >= q.length || q.length === 0) return true;
    const clampedTo = Math.max(0, Math.min(to, q.length - 1));
    if (from === clampedTo) return true;
    const next = [...q];
    const [moved] = next.splice(from, 1);
    next.splice(clampedTo, 0, moved);
    return commitQueues({ ...queues, [sessionId]: next }, set);
  },
  setStatus: (sessionId, id, status) => {
    const { queues } = get();
    const q = queueOf(queues, sessionId);
    if (!q.some((it) => it.id === id)) return true;
    return commitQueues({ ...queues, [sessionId]: q.map((it) => it.id === id
      ? { ...it, status, promptId: undefined, recovery: undefined, sendError: undefined } : it) }, set);
  },
  claimNext: (sessionId, promptId) => {
    let claimed: QueueItem | null = null;
    set((s) => {
      const q = queueOf(s.queues, sessionId);
      const idx = q.findIndex((it) => it.status === "queued");
      if (idx === -1) return s;
      const next = [...q];
      claimed = { ...next[idx], status: "sending", ...(promptId ? { promptId } : {}) };
      next[idx] = claimed;
      if (!writeDurable(OUTBOX_KEY, { ...s.queues, [sessionId]: next })) { claimed = null; return s; }
      return { queues: { ...s.queues, [sessionId]: next } };
    });
    return claimed;
  },
  claimById: (sessionId, id, promptId) => {
    let claimed: QueueItem | null = null;
    set((s) => {
      const q = queueOf(s.queues, sessionId);
      const idx = q.findIndex((it) => it.id === id && it.status === "queued");
      if (idx === -1) return s;
      const next = [...q];
      claimed = { ...next[idx], status: "sending", promptId };
      next[idx] = claimed;
      if (!writeDurable(OUTBOX_KEY, { ...s.queues, [sessionId]: next })) { claimed = null; return s; }
      return { queues: { ...s.queues, [sessionId]: next } };
    });
    return claimed;
  },
  pauseQueued: sessionId => set(state => {
    const queue = queueOf(state.queues, sessionId);
    if (!queue.some(item => item.status === "queued")) return state;
    return { queues: { ...state.queues, [sessionId]: queue.map(item => item.status === "queued"
      ? { ...item, status: "paused" as const } : item) } };
  }),
  settleSending: (sessionId, settlement, promptId) => {
    let settled: QueueItem | null = null;
    set((s) => {
      const q = queueOf(s.queues, sessionId);
      const idx = q.findIndex((it) => (
        (it.status === "sending" || (it.recovery === "uncertain" && !!promptId)) && (!promptId || it.promptId === promptId)
      ));
      if (idx === -1) return s;
      settled = q[idx];
      const next = settlement === "retry"
        ? q.map((it, itemIndex) => itemIndex === idx
          ? { ...it, status: "queued" as const, promptId: undefined }
          : it)
        : q.filter((_, itemIndex) => itemIndex !== idx);
      const queues = { ...s.queues };
      if (next.length === 0) delete queues[sessionId];
      else queues[sessionId] = next;
      return { queues };
    });
    return settled;
  },
  holdSending: (sessionId, promptId, uncertain = true, message) => set(state => {
    const queue = queueOf(state.queues, sessionId);
    if (!queue.some(item => item.status === "sending" && item.promptId === promptId)) return state;
    const next = queue.map(item => item.status === "sending" && item.promptId === promptId
      ? {...item, status: "paused" as const, recovery: uncertain ? "uncertain" as const : undefined,
        promptId: uncertain ? promptId : undefined, sendError: message} : item);
    const queues = {...state.queues, [sessionId]:next};
    writeDurable(OUTBOX_KEY, queues);
    return {queues};
  }),
  retryAllSending: () =>
    set((state) => {
      let changed = false;
      const queues = Object.fromEntries(
        Object.entries(state.queues).map(([sessionId, items]) => [
          sessionId,
          items.map((item) => {
            if (item.status !== "sending") return item;
            changed = true;
            return { ...item, status: "paused" as const, recovery: "uncertain" as const };
          }),
        ]),
      );
      return changed ? { queues } : state;
    }),
  shiftNext: (sessionId) => {
    const q = [...queueOf(get().queues, sessionId)];
    const idx = q.findIndex((it) => it.status === "queued");
    if (idx === -1) return null;
    const [item] = q.splice(idx, 1);
    const queues = { ...get().queues };
    if (q.length === 0) delete queues[sessionId];
    else queues[sessionId] = q;
    set({ queues });
    return item;
  },
  clear: (sessionId) =>
    set((s) => {
      if (!(sessionId in s.queues)) return s;
      const queues = { ...s.queues };
      delete queues[sessionId];
      return { queues };
    }),
  getQueue: (sessionId) => queueOf(get().queues, sessionId),
}));

/** 是否存在任意 active(非 paused)项 —— App 判定「回完一条是否自动续发」。 */
useMessageQueueStore.subscribe((state, previous) => {
  if (state.queues !== previous.queues) writeDurable(OUTBOX_KEY, state.queues);
});

export function hasActiveItems(q: QueueItem[]): boolean {
  return q.some((it) => it.status === "queued");
}

/**
 * Convert an agent terminal reason into explicit queue semantics.
 *
 * Cancellation consumes an already-admitted queue item (its user bubble is
 * present in history) but deliberately does not launch the next item. Failed
 * requests remain editable/retryable. Only a normal/unknown successful
 * terminal may advance to the next item.
 */
export function queueTerminalPolicy(
  stopReason: string,
  cancellationCategory?: string,
): QueueTerminalPolicy {
  if (
    [
      "rate_limit",
      "rate_limited",
      "error",
      "refusal",
      "content_filter",
      "max_tokens",
      "max_turns",
    ].includes(stopReason)
    || ["HookDenied", "max_turns_reached", "action_stationarity"]
      .includes(cancellationCategory ?? "")
  ) {
    return { settlement: "retry", autoAdvance: false, failed: true };
  }
  if (stopReason === "cancelled") {
    return { settlement: "consume", autoAdvance: false, failed: false };
  }
  return { settlement: "consume", autoAdvance: true, failed: false };
}
