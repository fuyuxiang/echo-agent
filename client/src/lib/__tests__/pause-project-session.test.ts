import { beforeEach, expect, it, vi } from "vitest";
import { pauseProjectSession } from "../pause-project-session";
import { agentCancel } from "../agent-client";
import { useSessionStore } from "@/stores/session-store";
import { useSessionsStore } from "@/stores/sessions-store";
import { useMessageQueueStore } from "@/stores/message-queue-store";
import { usePermissionStore } from "@/stores/permission-store";
import { useQuestionStore } from "@/stores/question-store";

vi.mock("../agent-client", () => ({ agentCancel: vi.fn() }));
beforeEach(() => {
  localStorage.clear(); vi.clearAllMocks();
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
