import { create } from "zustand";
import { agentListPendingInteractions } from "./agent-client";
import { isWaitingForUser } from "./turn-status";
import type { SessionStatus } from "./types";
import { usePermissionStore } from "@/stores/permission-store";
import { hasPendingQuestionForSession, useQuestionStore } from "@/stores/question-store";
import { useSessionStore, type SessionTranscript } from "@/stores/session-store";
import { useSessionsStore } from "@/stores/sessions-store";

const IPC_READ_TIMEOUT_MS = 5_000;
const RETRY_DELAYS_MS = [0, 250, 1_000];

interface InteractionRecovery {
  phase: "checking" | "failed";
  retry: () => Promise<void>;
  dispose: () => void;
}

const useInteractionRecoveries = create<Record<string, InteractionRecovery | undefined>>(() => ({}));

export function useSessionInteractionRecovery(sessionId: string) {
  return useInteractionRecoveries(state => state[sessionId]?.phase);
}

export function retrySessionInteractions(sessionId: string) {
  const recovery = useInteractionRecoveries.getState()[sessionId];
  if (recovery?.phase === "failed") void recovery.retry();
}

export function pendingSessionInteractionStatus(sessionId: string) {
  const pending = [
    ["awaiting_permission", !!usePermissionStore.getState().queues[sessionId]?.length],
    ["awaiting_answer", hasPendingQuestionForSession(sessionId)],
    ["awaiting_approval", !!useSessionStore.getState().transcripts[sessionId]?.planApprovals.length],
  ] as const;
  const current = useSessionsStore.getState().independent.find(
    (session) => session.sessionId === sessionId,
  )?.status;
  // Keep the interaction already presented until it closes.
  return pending.find(([status, waiting]) => waiting && status === current)?.[0]
    ?? pending.find(([, waiting]) => waiting)?.[0];
}

async function readPendingInteractions(sessionId: string, isCurrent: () => boolean) {
  // Local IPC can recover without an online event. Bound the number of
  // automatic attempts because a timed-out native invoke can stay in flight.
  for (const delay of RETRY_DELAYS_MS) {
    if (delay) await new Promise<void>(resolve => setTimeout(resolve, delay));
    if (!isCurrent()) return;
    const visible = {
      permissions: usePermissionStore.getState().queues[sessionId] ?? [],
      questions: useQuestionStore.getState().queues[sessionId] ?? [],
      planApprovals: useSessionStore.getState().transcripts[sessionId]?.planApprovals ?? [],
    };
    let timeoutId: ReturnType<typeof setTimeout> | undefined;
    try {
      const deadline = new Promise<never>((_, reject) => {
        timeoutId = setTimeout(() => reject(new Error("读取待处理交互超时")), IPC_READ_TIMEOUT_MS);
      });
      // Tauri invoke can't be cancelled. The expired race ignores its late reply.
      const pending = await Promise.race([agentListPendingInteractions(sessionId), deadline]);
      return { visible, pending };
    } catch {
      // A fresh snapshot can succeed after native IPC recovers.
    } finally {
      clearTimeout(timeoutId);
    }
  }
}

function reconcileRequests<T extends { sessionId: string; requestId: string }>(
  sessionId: string,
  visible: T[],
  pending: T[],
  add: (request: T) => void,
  close: (requestId: string, sessionId: string) => void,
) {
  const pendingIds = new Set<string>();
  for (const request of pending) {
    if (request.sessionId !== sessionId) continue;
    pendingIds.add(request.requestId);
    add(request);
  }
  // Only retire requests known before the read. Later live arrivals may not
  // have been captured by the native snapshot yet.
  for (const request of visible) {
    if (!pendingIds.has(request.requestId)) close(request.requestId, sessionId);
  }
}

function activePromptId(transcript: SessionTranscript) {
  return transcript.pendingSendNowPromptId ?? transcript.messages.find(
    message => message.id === transcript.streamingMessageId,
  )?.promptId;
}

/** Recover reverse requests suppressed while a failed pause or stop was pending. */
export async function restoreSessionInteractions(
  sessionId: string,
  fallbackStatus: SessionStatus,
): Promise<void> {
  const before = useSessionStore.getState().transcripts[sessionId];
  if (!before || before.control || useSessionsStore.getState().deletedSessionIds.includes(sessionId)) return;

  const promptId = activePromptId(before);
  const { streamingMessageId, completedPromptIds } = before;
  const ownsRecovery = () => useInteractionRecoveries.getState()[sessionId]?.retry === retry;
  const isCurrent = () => {
    const transcript = useSessionStore.getState().transcripts[sessionId];
    if (!ownsRecovery() || !transcript || transcript.control
      || useSessionsStore.getState().deletedSessionIds.includes(sessionId)) return false;
    // Send Now keeps one prompt ID across retirement of the old bubble and
    // admission of the new one. A legacy turn can still be tracked by its bubble.
    if (promptId) return activePromptId(transcript) === promptId;
    return !transcript.pendingSendNowPromptId && transcript.streamingMessageId === streamingMessageId
      && (streamingMessageId !== null || transcript.completedPromptIds === completedPromptIds);
  };
  const retireIfStale = () => { if (!isCurrent()) dispose(); };
  const unsubscribeTranscript = useSessionStore.subscribe((state, previous) => {
    if (state.transcripts[sessionId] !== previous.transcripts[sessionId]) retireIfStale();
  });
  const unsubscribeCatalog = useSessionsStore.subscribe((state, previous) => {
    if (state.deletedSessionIds !== previous.deletedSessionIds) retireIfStale();
  });

  function dispose() {
    unsubscribeTranscript();
    unsubscribeCatalog();
    if (ownsRecovery()) {
      const next = { ...useInteractionRecoveries.getState() };
      delete next[sessionId];
      useInteractionRecoveries.setState(next, true);
    }
  }

  async function retry() {
    if (!isCurrent()) return;
    useInteractionRecoveries.setState({ [sessionId]: { retry, dispose, phase: "checking" } });
    const snapshot = await readPendingInteractions(sessionId, isCurrent);
    if (!isCurrent()) return;
    if (!snapshot) {
      useInteractionRecoveries.setState({ [sessionId]: { retry, dispose, phase: "failed" } });
      return;
    }

    const { visible, pending } = snapshot;
    const { request: addPermission, close } = usePermissionStore.getState();
    const { request: addQuestion, dismiss } = useQuestionStore.getState();
    const { requestPlanApproval, dismissPlanApproval } = useSessionStore.getState();
    reconcileRequests(sessionId, visible.permissions, pending.permissions, addPermission, close);
    reconcileRequests(sessionId, visible.questions, pending.questions, addQuestion, dismiss);
    reconcileRequests(sessionId, visible.planApprovals, pending.planApprovals, requestPlanApproval, dismissPlanApproval);
    const currentStatus = useSessionsStore.getState().independent.find(session => session.sessionId === sessionId)?.status;
    const status = pendingSessionInteractionStatus(sessionId)
      ?? (isWaitingForUser(currentStatus) ? fallbackStatus : undefined);
    if (status && status !== currentStatus) useSessionsStore.getState().upsert({
      sessionId,
      status,
      updatedAt: new Date().toISOString(),
    });
    dispose();
  }

  useInteractionRecoveries.getState()[sessionId]?.dispose();
  useInteractionRecoveries.setState({ [sessionId]: { retry, dispose, phase: "checking" } });
  useSessionsStore.getState().upsert({
    sessionId,
    status: pendingSessionInteractionStatus(sessionId) ?? fallbackStatus,
  });
  await retry();
}
