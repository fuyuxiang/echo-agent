import { useCallback, useEffect, useRef } from "react";
import {
  useMessageQueueStore,
  queueTerminalPolicy,
  type QueueItem,
  type CodingQueuedContext,
} from "@/stores/message-queue-store";
import { useSessionStore } from "@/stores/session-store";
import {
  codingAdmissionKey,
  codingQueueId,
  useCodingAdmissionStore,
  type QueuedAdmissionResult,
} from "../lib/coding-followup";

interface Options {
  workspace: string;
  taskId?: string;
  sessionId?: string | null;
  /** Automatic admission belongs to the persistent task runtime. */
  autoReady: boolean;
  manualReady: boolean;
  send: (
    text: string,
    contextPaths: string[],
    promptId: string,
    manual: boolean,
    coding?: CodingQueuedContext,
  ) => Promise<QueuedAdmissionResult>;
  onToast?: (message: string) => void;
}
const EMPTY: QueueItem[] = [];
// A foreground hook and the persistent runtime can coexist for the same owner.
const inFlight = new Set<string>();

export function useCodingFollowupQueue(options: Options) {
  const queueId = codingQueueId(
    options.workspace,
    options.taskId,
    options.sessionId,
  );
  const admissionBusy = useCodingAdmissionStore(
    (state) =>
      !!state.active[codingAdmissionKey(options.workspace, options.taskId)],
  );
  const queue = useMessageQueueStore((state) =>
    queueId ? (state.queues[queueId] ?? EMPTY) : EMPTY,
  );
  const messages = useSessionStore((state) =>
    options.sessionId
      ? state.transcripts[options.sessionId]?.messages
      : undefined,
  );
  const current = useRef({ ...options, queueId });
  current.current = {
    ...options,
    autoReady: options.autoReady && !admissionBusy,
    manualReady: options.manualReady && !admissionBusy,
    queueId,
  };

  // Local admission is not a native acknowledgement. Keep the outbox row until
  // its exact prompt completes; failures remain paused and editable after restart.
  useEffect(() => {
    if (!queueId) return;
    const store = useMessageQueueStore.getState();
    for (const item of queue) {
      if (item.status !== "sending" || !item.promptId) continue;
      const terminal = messages?.find(
        (message) =>
          message.role === "assistant" &&
          message.complete &&
          message.promptId === item.promptId &&
          message.stopReason,
      );
      if (!terminal) continue;
      if (
        queueTerminalPolicy(terminal.stopReason!, terminal.cancellationCategory)
          .failed
      )
        store.holdSending(
          queueId,
          item.promptId,
          true,
          "本轮执行失败，请检查任务记录后再恢复。",
        );
      else store.settleSending(queueId, "consume", item.promptId);
    }
  }, [messages, queue, queueId]);

  const dispatch = useCallback(async (itemId?: string) => {
    const owner = current.current;
    const key = owner.queueId;
    if (
      !key ||
      !(itemId ? owner.manualReady : owner.autoReady) ||
      inFlight.has(key)
    )
      return false;
    const store = useMessageQueueStore.getState();
    const rows = store.getQueue(key);
    if (
      rows.some((item) => item.status === "sending") ||
      (!itemId &&
        rows.some((item) => item.recovery === "uncertain" || item.sendError))
    )
      return false;
    inFlight.add(key);
    const promptId = crypto.randomUUID();
    const item = itemId
      ? store.claimById(key, itemId, promptId)
      : store.claimNext(key, promptId);
    if (!item) {
      inFlight.delete(key);
      return false;
    }
    try {
      const accepted = await owner.send(
        item.text,
        item.attachments ?? [],
        promptId,
        Boolean(itemId),
        item.coding,
      );
      if (accepted === "deferred") {
        store.settleSending(key, "retry", promptId);
        return false;
      }
      if (!accepted)
        store.holdSending(
          key,
          promptId,
          false,
          "当前任务暂不能发送，请处理任务状态或待确认事项后恢复。",
        );
      return accepted;
    } catch (error) {
      store.holdSending(
        key,
        promptId,
        false,
        `未发送：${String(error).replace(/^Error:\s*/, "")}`,
      );
      if (current.current.queueId === key)
        owner.onToast?.("队列暂未发送，已保留。请检查任务状态后重试。");
      return false;
    } finally {
      inFlight.delete(key);
    }
  }, []);

  useEffect(() => {
    if (options.autoReady && queue.some((item) => item.status === "queued"))
      void dispatch();
  }, [dispatch, options.autoReady, admissionBusy, queue, queueId]);

  const enqueue = useCallback(
    (text: string, contextPaths: string[], coding?: CodingQueuedContext) => {
      const key = current.current.queueId;
      if (!key) return false;
      useMessageQueueStore.getState().enqueue(key, text, contextPaths, coding);
      return true;
    },
    [],
  );
  const sending = queue.some((item) => item.status === "sending");
  return {
    queueId,
    enqueue,
    sendItem: dispatch,
    manualReady: options.manualReady && !admissionBusy && !sending,
    sending: sending || admissionBusy,
  };
}
