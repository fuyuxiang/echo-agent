import { agentCancel } from "./agent-client";
import { sessionControlReceiptState, useSessionStore } from "@/stores/session-store";
import { useSessionsStore } from "@/stores/sessions-store";
import { useMessageQueueStore } from "@/stores/message-queue-store";
import { usePermissionStore } from "@/stores/permission-store";
import { useQuestionStore } from "@/stores/question-store";

/** Exactly the selected session is fenced; acknowledge only after native cancellation. */
export async function pauseProjectSession(sessionId: string) {
  const transcript = useSessionStore.getState().transcripts[sessionId];
  const previousStatus = useSessionsStore.getState().independent.find(s => s.sessionId === sessionId)?.status;
  const activePromptId = transcript?.messages.find(m => m.id === transcript.streamingMessageId)?.promptId;
  const promptId = transcript?.pendingSendNowPromptId ?? activePromptId;
  const request = useSessionStore.getState().requestControl(sessionId, "pause", promptId);
  useSessionsStore.getState().upsert({ sessionId, status: "pausing" });
  try {
    if (transcript?.pendingSendNowPromptId && activePromptId) {
      await agentCancel(sessionId, "pause", promptId, activePromptId);
    } else {
      await agentCancel(sessionId, "pause", promptId);
    }
    const receipt = sessionControlReceiptState(sessionId, request);
    if (!receipt) return;
    if (receipt === "pending") useSessionStore.getState().confirmControl(sessionId, "pause");
    useMessageQueueStore.getState().pauseQueued(sessionId);
    useMessageQueueStore.getState().settleSending(sessionId, "consume", promptId);
    useSessionsStore.getState().upsert({ sessionId, status: "paused", updatedAt: new Date().toISOString() });
    const permissions = usePermissionStore.getState();
    for (const r of permissions.queues[sessionId] ?? []) permissions.close(r.requestId, sessionId);
    const questions = useQuestionStore.getState();
    for (const r of questions.queues[sessionId] ?? []) questions.dismiss(r.requestId, sessionId);
  } catch (error) {
    const receipt = sessionControlReceiptState(sessionId, request);
    if (!receipt) return;
    if (receipt === "pending") {
      useSessionStore.getState().rejectControl(sessionId, "pause");
      useSessionsStore.getState().upsert({ sessionId, status: previousStatus ?? (transcript?.streamingMessageId ? "working" : "pending") });
    }
    throw error;
  }
}
