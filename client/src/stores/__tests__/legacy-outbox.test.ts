import { afterEach, expect, it, vi } from "vitest";

afterEach(() => {
  localStorage.clear();
  vi.resetModules();
});

it("retains retired queue records and ordinary drafts without reviving a retired dispatch", async () => {
  const key = "echoagent.outbox.v1";
  const owner = 'coding:["/work","task","session"]';
  const old = { id: "retired", text: "旧草稿", attachments: ["/work/资料.png"], status: "sending", promptId: "old-run", createdAt: 1 };
  const oldContext = { ...old, id: "old-context", coding: { mode: "plan", editorContext: "草稿上下文" } };
  const normal = { id: "normal", text: "待确认的普通消息", status: "queued", createdAt: 2 };
  localStorage.setItem(key, JSON.stringify({ version: 1, data: { [owner]: [old], session: [oldContext, normal] } }));

  const { useMessageQueueStore } = await import("../message-queue-store");
  const queue = useMessageQueueStore.getState();
  expect(queue.getQueue(owner)).toEqual([]);
  expect(queue.getQueue("session")).toEqual([expect.objectContaining({ id: "normal", status: "paused" })]);
  expect(queue.claimNext(owner)).toBeNull();
  expect(queue.claimNext("session")).toBeNull();

  queue.setStatus("session", normal.id, "queued");
  expect(queue.claimNext("session", "normal-run")?.id).toBe(normal.id);
  queue.settleSending("session", "consume", "normal-run");
  const persisted = JSON.parse(localStorage.getItem(key)!);
  expect(persisted.data).toEqual({ [owner]: [old], session: [oldContext] });

  vi.resetModules();
  const restarted = (await import("../message-queue-store")).useMessageQueueStore.getState();
  expect(restarted.getQueue(owner)).toEqual([]);
  expect(restarted.getQueue("session")).toEqual([]);
  expect(JSON.parse(localStorage.getItem(key)!)).toEqual(persisted);
});
