import { beforeEach, expect, it } from "vitest";
import { useSessionStore } from "../session-store";
import { useMessageQueueStore } from "../message-queue-store";
import { codingQueueId } from "@/lib/queue-owner";

beforeEach(() => {
  localStorage.clear();
  useMessageQueueStore.setState({ queues: {} });
  useSessionStore.setState(useSessionStore.getInitialState());
});

function prepare(sessionId = "s1", promptId = "p1", key = sessionId) {
  const queue = useMessageQueueStore.getState();
  const id = queue.enqueue(key, "1123", ["/work/附件.docx"]);
  queue.claimById(key, id, promptId);
  const chat = useSessionStore.getState();
  chat.setSession(sessionId);
  chat.pushUser("1123", ["/work/附件.docx"], sessionId);
  chat.startStreaming(sessionId, promptId);
  return id;
}

it.each(["user_message_chunk", "agent_message_chunk", "agent_thought_chunk", "tool_call"])(
  "only actual live %s confirms a claimed prompt; local optimistic admission does not", sessionUpdate => {
    prepare();
    const queue = useMessageQueueStore.getState();
    expect(queue.getQueue("s1")[0].status).toBe("sending");
    useSessionStore.getState().applyUpdate({ __sessionId: "s1", sessionUpdate,
      content: { type: "text", text: sessionUpdate === "user_message_chunk" ? "1123" : "正在执行" },
      toolCallId: "tool", title: "读取资料", status: "in_progress", _meta: { promptId: "p1" },
    } as Parameters<ReturnType<typeof useSessionStore.getState>["applyUpdate"]>[0]);
    expect(queue.getQueue("s1")[0]).toMatchObject({ status: "accepted", promptId: "p1", attachments: ["/work/附件.docx"] });
  },
);

it("replay, older prompt output, unrelated sessions and old cancellations do not acknowledge a replacement", () => {
  prepare("s1", "old");
  const queue = useMessageQueueStore.getState();
  const id = queue.enqueue("s1", "新要求");
  queue.claimById("s1", id, "replacement");
  const chat = useSessionStore.getState();
  chat.requestSendNow("replacement", "s1");
  for (const update of [
    { __sessionId: "s1", sessionUpdate: "user_message_chunk", content: { type: "text", text: "新要求" }, _meta: { promptId: "replacement", isReplay: true } },
    { __sessionId: "s1", sessionUpdate: "agent_message_chunk", content: { type: "text", text: "旧输出" }, _meta: { promptId: "old" } },
    { __sessionId: "s2", sessionUpdate: "user_message_chunk", content: { type: "text", text: "新要求" }, _meta: { promptId: "replacement" } },
    { __sessionId: "s1", sessionUpdate: "turn_completed", prompt_id: "old", stop_reason: "cancelled", _meta: { cancelTrigger: "send_now" } },
  ] as const) chat.applyUpdate(update);
  expect(queue.getQueue("s1").find(item => item.id === id)?.status).toBe("sending");
  chat.applyUpdate({ __sessionId: "s1", sessionUpdate: "user_message_chunk", content: { type: "text", text: "新要求" }, _meta: { promptId: "replacement" } });
  expect(queue.getQueue("s1").find(item => item.id === id)?.status).toBe("accepted");
});

it("a legacy user echo acknowledges the newly activated send-now identity", () => {
  prepare("s1", "old");
  const queue = useMessageQueueStore.getState();
  const id = queue.enqueue("s1", "新要求");
  queue.claimById("s1", id, "replacement");
  const chat = useSessionStore.getState();
  chat.requestSendNow("replacement", "s1");
  chat.markComplete({ sessionId: "s1", promptId: "old", stopReason: "cancelled", cancelTrigger: "send_now" });
  chat.applyUpdate({ __sessionId: "s1", sessionUpdate: "user_message_chunk", content: { type: "text", text: "新要求" } });
  expect(queue.getQueue("s1").find(item => item.id === id)?.status).toBe("accepted");
  expect(useSessionStore.getState().sendNowPending).toBe(false);
});

it("background coding admission uses the original workspace/task/session, preserving the focused conversation", () => {
  const key = codingQueueId("/repo", "A", "background")!;
  prepare("background", "p1", key);
  useSessionStore.getState().setSession("focused");
  useSessionStore.getState().applyUpdate({ __sessionId: "background", sessionUpdate: "user_message_chunk",
    content: { type: "text", text: "1123" }, _meta: { promptId: "p1" } });
  expect(useMessageQueueStore.getState().getQueue(key)[0].status).toBe("accepted");
  expect(useSessionStore.getState().sessionId).toBe("focused");
  expect(useSessionStore.getState().messages).toEqual([]);
});

it.each(["end_turn", "error"].flatMap(stopReason => [true, false].map(identified => ({ stopReason, identified }))))(
  "durable $stopReason (identified=$identified) settles ownership if the public completion is lost", ({ stopReason, identified }) => {
  const id = prepare();
  const queue = useMessageQueueStore.getState();
  queue.enqueue("s1", "下一条");
  const chat = useSessionStore.getState();
  chat.applyUpdate({ __sessionId: "s1", sessionUpdate: "agent_message_chunk", content: { type: "text", text: "已经执行" }, _meta: { promptId: "p1" } });
  chat.applyUpdate({ __sessionId: "s1", sessionUpdate: "turn_completed", ...(identified ? { prompt_id: "p1" } : {}), stop_reason: stopReason });
  if (stopReason === "end_turn") {
    expect(queue.getQueue("s1")).toMatchObject([{ text: "下一条", status: "queued" }]);
  } else {
    expect(queue.getQueue("s1")).toMatchObject([{ id, status: "paused", recovery: "failed" }, { text: "下一条", status: "paused" }]);
  }
  expect(queue.getQueue("s1").some(item => item.status === "accepted")).toBe(false);
});

it("late durable failure settles its old row without pausing newer follow-ups", () => {
  prepare("s1", "old");
  const queue = useMessageQueueStore.getState();
  queue.acknowledgePrompt("s1", "old");
  const next = queue.enqueue("s1", "替代轮");
  queue.claimById("s1", next, "current");
  queue.enqueue("s1", "下一条");
  useSessionStore.getState().startStreaming("s1", "current");
  useSessionStore.getState().applyUpdate({ __sessionId: "s1", sessionUpdate: "turn_completed", prompt_id: "old", stop_reason: "error" });
  expect(queue.getQueue("s1").find(item => item.text === "下一条")?.status).toBe("queued");
  expect(queue.getQueue("s1").find(item => item.id === next)?.status).toBe("sending");
});
