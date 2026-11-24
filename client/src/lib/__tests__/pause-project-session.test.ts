import { beforeEach, expect, it, vi } from "vitest";
import { pauseProjectSession } from "../pause-project-session";
import { agentCancel, agentListPendingInteractions } from "../agent-client";
import { useSessionStore } from "@/stores/session-store";
import { useSessionsStore } from "@/stores/sessions-store";
import { useMessageQueueStore } from "@/stores/message-queue-store";
import { usePermissionStore } from "@/stores/permission-store";
import { useQuestionStore, type QuestionRequest } from "@/stores/question-store";

vi.mock("../agent-client", () => ({
  agentCancel: vi.fn(),
  agentListPendingInteractions: vi.fn(),
}));
beforeEach(() => {
  localStorage.clear(); vi.clearAllMocks();
  vi.mocked(agentListPendingInteractions).mockResolvedValue({
    permissions: [], questions: [], planApprovals: [], folderTrustRequests: [],
  });
  useSessionStore.setState({ ...useSessionStore.getInitialState(), transcripts: {} });
  useSessionsStore.setState({ independent: [{ sessionId: "target", title: "后台任务", cwd: "/work", status: "working" }, { sessionId: "other", title: "当前任务", cwd: "/else", status: "working" }] });
  useMessageQueueStore.setState({ queues: {} });
  usePermissionStore.getState().clearAll(); useQuestionStore.getState().clearAll();
  useSessionStore.getState().setSession("target");
  useSessionStore.getState().startStreaming("target", "prompt-target");
  useSessionStore.getState().setSession("other");
  useSessionStore.getState().startStreaming("other", "prompt-other");
});

it("pauses only the selected background session after the native acknowledgement", async () => {
  let acknowledge!: () => void;
  vi.mocked(agentCancel).mockReturnValue(new Promise(resolve => { acknowledge = resolve; }));
  useMessageQueueStore.getState().enqueue("target", "next");
  useMessageQueueStore.getState().enqueue("other", "untouched");
  const pending = pauseProjectSession("target");
  expect(agentCancel).toHaveBeenCalledWith("target", "pause", "prompt-target");
  expect(useSessionsStore.getState().independent.find(s => s.sessionId === "target")?.status).toBe("pausing");
  expect(useSessionStore.getState().transcripts.target.control?.phase).toBe("pausing");
  acknowledge(); await pending;
  expect(useSessionStore.getState().transcripts.target.control?.phase).toBe("paused");
  expect(useSessionsStore.getState().independent.find(s => s.sessionId === "target")?.status).toBe("paused");
  expect(useSessionStore.getState().sessionId).toBe("other");
  expect(useSessionStore.getState().transcripts.other.streamingMessageId).toBeTruthy();
  expect(useMessageQueueStore.getState().queues.other[0].status).toBe("queued");
});

it("keeps a running task honest after cancellation fails", async () => {
  vi.mocked(agentCancel).mockRejectedValue(new Error("runtime unavailable"));
  await expect(pauseProjectSession("target")).rejects.toThrow("runtime unavailable");
  expect(useSessionStore.getState().transcripts.target.control).toBeUndefined();
  expect(useSessionsStore.getState().independent.find(s => s.sessionId === "target")?.status).toBe("working");
  expect(useSessionStore.getState().transcripts.target.streamingMessageId).toBeTruthy();
});

it.each([true, false])("clears a closed question after a failed pause, with close event: %s", async (closedEvent) => {
  useSessionsStore.getState().upsert({ sessionId: "target", status: "awaiting_answer" });
  useQuestionStore.getState().request({
    sessionId: "target", requestId: "resolved", toolCallId: "ask", title: "需要确认", mode: "default", questions: [],
  });
  let rejectPause!: (error: Error) => void;
  vi.mocked(agentCancel).mockReturnValue(new Promise((_, reject) => { rejectPause = reject; }));
  const pausing = pauseProjectSession("target");
  if (closedEvent) useQuestionStore.getState().dismiss("resolved", "target");
  rejectPause(new Error("runtime unavailable"));
  await expect(pausing).rejects.toThrow("runtime unavailable");
  expect(useQuestionStore.getState().queues.target ?? []).toEqual([]);
  expect(useSessionsStore.getState().independent.find(s => s.sessionId === "target")?.status).toBe("working");
});

it.each([false, true])("restores the selected background session's pending question after pause fails, retry=%s", async (retry) => {
  const question: QuestionRequest = {
    sessionId: "target", requestId: "question-target", toolCallId: "ask",
    title: "需要确认", mode: "default", questions: [],
  };
  vi.mocked(agentListPendingInteractions).mockResolvedValue({
    permissions: [], planApprovals: [], folderTrustRequests: [],
    questions: [
      question,
      { ...question, sessionId: "other", requestId: "foreign-question" },
    ],
  });
  vi.mocked(agentCancel).mockRejectedValue(new Error("runtime unavailable"));
  if (retry) vi.mocked(agentListPendingInteractions).mockRejectedValueOnce(new Error("IPC unavailable"));

  vi.useFakeTimers();
  try {
    await expect(pauseProjectSession("target")).rejects.toThrow("runtime unavailable");
    if (retry) {
      expect(useQuestionStore.getState().queues.target).toBeUndefined();
      await vi.advanceTimersByTimeAsync(1_000);
    }
  } finally { vi.useRealTimers(); }
  expect(agentListPendingInteractions).toHaveBeenCalledWith("target");
  expect(useQuestionStore.getState().queues.target).toMatchObject([{ requestId: "question-target" }]);
  expect(useQuestionStore.getState().queues.other).toBeUndefined();
  expect(useSessionsStore.getState().independent.map(({ status }) => status))
    .toEqual(["awaiting_answer", "working"]);
  expect(useSessionStore.getState().sessionId).toBe("other");
  expect(useSessionStore.getState().streaming).toBe(true);
});

it("keeps a pause confirmed by the terminal event before IPC returns", async () => {
  let acknowledge!: () => void;
  vi.mocked(agentCancel).mockReturnValue(new Promise(resolve => { acknowledge = resolve; }));
  useMessageQueueStore.getState().enqueue("target", "later");
  const pending = pauseProjectSession("target");
  useSessionStore.getState().markComplete({
    sessionId: "target", promptId: "prompt-target", stopReason: "cancelled", cancelTrigger: "pause",
  });
  acknowledge();
  await pending;
  expect(useSessionStore.getState().transcripts.target.control).toMatchObject({ phase: "paused", promptId: "prompt-target" });
  expect(useSessionsStore.getState().independent.find(session => session.sessionId === "target")?.status).toBe("paused");
  expect(useMessageQueueStore.getState().getQueue("target")).toMatchObject([{ status: "paused" }]);
});

it("preserves a replacement prompt and its queue when an old pause is acknowledged", async () => {
  let acknowledge!: () => void;
  vi.mocked(agentCancel).mockReturnValue(new Promise(resolve => { acknowledge = resolve; }));
  const pending = pauseProjectSession("target");
  useSessionStore.getState().startStreaming("target", "replacement");
  useSessionsStore.getState().upsert({ sessionId: "target", status: "working" });
  useMessageQueueStore.getState().enqueue("target", "replacement follow-up");
  acknowledge();
  await pending;
  const transcript = useSessionStore.getState().transcripts.target;
  expect(transcript.messages.find(message => message.id === transcript.streamingMessageId))
    .toMatchObject({ promptId: "replacement", complete: false });
  expect(useSessionsStore.getState().independent.find(session => session.sessionId === "target")?.status).toBe("working");
  expect(useMessageQueueStore.getState().getQueue("target")).toMatchObject([{ status: "queued" }]);
});
