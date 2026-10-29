import { beginAgentTurn } from "@/lib/agent-turn";
import { useSessionStore } from "@/stores/session-store";
import { usePermissionStore } from "@/stores/permission-store";
import { useQuestionStore } from "@/stores/question-store";
import {
  useMessageQueueStore,
  queueTerminalPolicy,
  type CodingQueuedContext,
} from "@/stores/message-queue-store";
import type { PromptComplete } from "@/lib/types";
import { codingApi } from "./tauri-api";
import { buildCodingWorkflowPrompt } from "./workflow";
import type { CodingTask, TaskPhase } from "./types";
import { create } from "zustand";
import { isBusyPhase } from "./phase";

export type QueuedAdmissionResult = boolean | "deferred";

export const useCodingAdmissionStore = create<{
  active: Record<string, symbol>;
}>(() => ({ active: {} }));
export const codingAdmissionKey = (root: string, taskId?: string) =>
  JSON.stringify([root, taskId]);

/** Manual input and the background queue share one task admission boundary. */
export function claimCodingAdmission(
  root: string,
  taskId: string,
): (() => void) | null {
  const key = codingAdmissionKey(root, taskId);
  if (useCodingAdmissionStore.getState().active[key]) return null;
  const owner = Symbol();
  useCodingAdmissionStore.setState((state) => ({
    active: { ...state.active, [key]: owner },
  }));
  return () =>
    useCodingAdmissionStore.setState((state) => {
      if (state.active[key] !== owner) return state;
      const active = { ...state.active };
      delete active[key];
      return { active };
    });
}

export const MANUAL_FOLLOWUP_PHASES: TaskPhase[] = [
  "delivered",
  "paused",
  "stopped",
  "blocked",
  "discovering",
  "implementing",
  "repairing",
];

export function codingQueueId(
  root: string,
  taskId?: string,
  sessionId?: string | null,
): string | null {
  return taskId && sessionId
    ? `coding:${JSON.stringify([root, taskId, sessionId])}`
    : null;
}

export function codingQueueOwner(
  key: string,
): { root: string; taskId: string; sessionId: string } | null {
  if (!key.startsWith("coding:")) return null;
  try {
    const owner: unknown = JSON.parse(key.slice(7));
    return Array.isArray(owner) &&
      owner.length === 3 &&
      owner.every((value) => typeof value === "string" && value.length > 0)
      ? { root: owner[0], taskId: owner[1], sessionId: owner[2] }
      : null;
  } catch {
    return null;
  }
}

/** Legacy identity fallback requires the transcript before an empty turn is discarded. */
export function settleCodingQueueTurn(completion: PromptComplete): void {
  const sessionId = completion.sessionId;
  if (!sessionId) return;
  const transcript = useSessionStore.getState().transcripts[sessionId];
  const promptId =
    completion.promptId ??
    transcript?.messages.find(
      (message) => message.id === transcript.streamingMessageId,
    )?.promptId;
  if (!promptId) return;
  const store = useMessageQueueStore.getState();
  for (const [key, items] of Object.entries(store.queues)) {
    if (
      codingQueueOwner(key)?.sessionId !== sessionId ||
      !items.some((item) => item.promptId === promptId)
    )
      continue;
    if (
      queueTerminalPolicy(
        completion.stopReason,
        completion.cancellationCategory,
      ).failed
    ) {
      store.holdSending(
        key,
        promptId,
        true,
        "本轮执行失败，请检查任务记录后再恢复。",
      );
    } else store.settleSending(key, "consume", promptId);
  }
}

/** Recheck the session at every asynchronous boundary, including background sends. */
export function codingSessionReady(sessionId: string): boolean {
  const transcript = useSessionStore.getState().transcripts[sessionId];
  return (
    !!transcript &&
    !transcript.streamingMessageId &&
    !transcript.pendingSendNowPromptId &&
    !transcript.planApprovals?.length &&
    !usePermissionStore.getState().queues[sessionId]?.length &&
    !useQuestionStore.getState().queues[sessionId]?.length
  );
}

/** One admission path for foreground/manual and background/automatic queued work. */
export async function sendCodingQueuedFollowup(options: {
  root: string;
  taskId: string;
  sessionId: string;
  promptId: string;
  text: string;
  contextPaths: string[];
  manual: boolean;
  coding?: CodingQueuedContext;
  canDispatch: () => boolean;
  prepare?: () => Promise<unknown>;
  onChanged?: () => Promise<unknown>;
}): Promise<QueuedAdmissionResult> {
  const {
    root,
    taskId,
    sessionId,
    promptId,
    text,
    contextPaths,
    manual,
    canDispatch,
    onChanged,
  } = options;
  const ready = () =>
    canDispatch() &&
    codingSessionReady(sessionId) &&
    (manual || !useSessionStore.getState().transcripts[sessionId]?.control);
  if (!ready()) return false;
  const release = claimCodingAdmission(root, taskId);
  if (!release) return "deferred";
  try {
    if (options.prepare) await options.prepare();
    if (!ready()) return false;
    const { task } = await codingApi.orchestratorState(root, taskId);
    if (!ready() || task.sessionId !== sessionId) return false;
    if (
      !(manual
        ? MANUAL_FOLLOWUP_PHASES.includes(task.phase)
        : task.phase === "delivered")
    ) {
      await onChanged?.().catch(() => undefined);
      // A phase event can arrive after a very fast completion. Refresh the
      // owner and wait for delivery instead of pausing a healthy queued row.
      return isBusyPhase(task.phase) ? "deferred" : false;
    }
    let reopened: CodingTask | undefined;
    let admitted = false;
    try {
      if (["paused", "stopped", "blocked", "delivered"].includes(task.phase)) {
        reopened = await codingApi.beginFollowup(root, taskId, text);
      }
      if (!ready()) return false;
      admitted = beginAgentTurn({
        sessionId,
        promptId,
        displayText: text,
        promptText: buildCodingWorkflowPrompt(text, contextPaths, true, {
          intent: options.coding?.mode,
          editorContext: options.coding?.editorContext,
        }),
        allowBackgroundSession: true,
        coding: {
          mode: options.coding?.mode ?? "agent",
          documentToolBridge: true,
          workspace: root,
          taskId,
        },
        attachments: options.coding?.imagePaths,
        onRejected: () =>
          settleCodingQueueTurn({ sessionId, promptId, stopReason: "error" }),
      });
      return admitted;
    } finally {
      try {
        if (reopened && !admitted) {
          // Navigation can revoke sending, but never revoke this admission's
          // responsibility to settle its original task. Native revision checks
          // keep a concurrent stop or a newer round authoritative.
          try {
            await codingApi.reportInterrupted(
              root,
              taskId,
              "paused",
              reopened.updatedAt,
            );
          } catch (error) {
            const latest = await codingApi.orchestratorState(root, taskId);
            if (latest.task.updatedAt === reopened.updatedAt) throw error;
          }
        }
      } finally {
        // Refresh failures cannot turn an admitted prompt into a second submission.
        await onChanged?.().catch(() => undefined);
      }
    }
  } finally {
    release();
  }
}
