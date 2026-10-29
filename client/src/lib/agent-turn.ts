import { agentSend, type CodingTurnOptions } from "@/lib/agent-client";
import { friendlyError } from "@/lib/error-format";
import { sessionControlStatus } from "@/lib/session-control";
import { useSessionsStore } from "@/stores/sessions-store";
import { useSessionStore } from "@/stores/session-store";
import { useMessageQueueStore } from "@/stores/message-queue-store";

export type AgentTurnSender = (
  sessionId: string,
  promptText: string,
  attachments: string[],
  displayText: string,
  promptId: string,
  coding?: CodingTurnOptions,
) => Promise<void>;

export interface AgentTurnInput {
  sessionId: string;
  promptText: string;
  displayText: string;
  attachments?: string[];
  coding?: CodingTurnOptions;
  /** Reuse the id assigned while atomically claiming a queued row. */
  promptId?: string;
  /** Message replacement may finish after navigation and must remain scoped to
   * its original session. Ordinary composer sends never enable this. */
  allowBackgroundSession?: boolean;
  /** Observe asynchronous native rejection without delaying local admission. */
  onRejected?: (error: unknown, promptId: string) => void;
}

let promptSequence = 0;

/** Stable across the optimistic placeholder and both completion event rails. */
export function createAgentPromptId(): string {
  const randomUUID = globalThis.crypto?.randomUUID?.bind(globalThis.crypto);
  if (randomUUID) return randomUUID();
  promptSequence += 1;
  return `desktop-${Date.now()}-${promptSequence}`;
}

/**
 * `prompt_complete` is delivered before the long-running ACP request settles.
 * A later transport rejection must not overwrite that authoritative terminal
 * event or cause an already-executed queue row to be sent again.
 */
export function isAgentPromptSettled(sessionId: string, promptId: string): boolean {
  const transcript = useSessionStore.getState().transcripts[sessionId];
  if (!transcript) return true;
  const matching = transcript.messages.filter(
    (message) => message.role === "assistant" && message.promptId === promptId,
  );
  if (matching.some((message) => message.complete)) return true;
  return transcript.pendingSendNowPromptId !== promptId
    && !matching.some((message) => !message.complete);
}

/**
 * Admit a user turn locally and observe the long-running ACP request without
 * returning its completion promise to the composer.
 *
 * ACP's PromptRequest resolves after the whole model turn, not when the prompt
 * is merely accepted. Keeping that promise out of the submit path lets the UI
 * clear immediately while the transcript continues streaming normally.
 */
export function beginAgentTurn(
  input: AgentTurnInput,
  send: AgentTurnSender = agentSend,
): boolean {
  const { sessionId, promptText, displayText } = input;
  const attachments = input.attachments ?? [];
  const promptId = input.promptId ?? createAgentPromptId();
  const transcript = useSessionStore.getState();
  if (transcript.sessionId !== sessionId && !input.allowBackgroundSession) {
    return false;
  }
  const targetTranscript = transcript.transcripts[sessionId];
  // startStreaming deliberately clears a paused/stopped barrier because a new
  // prompt is an explicit continue action. Keep the prior value until native
  // admission succeeds so an asynchronous rejection can restore honest UI.
  const previousControl = targetTranscript?.control;

  useSessionsStore.getState().upsert({ sessionId, status: "working" });
  // This records the admitted turn, not the next composer selection. Runtime
  // receives the same immutable mode, including background/queued turns.
  if (input.coding?.mode) transcript.setAgentMode(input.coding.mode === "agent" ? "default" : input.coding.mode, sessionId);
  transcript.pushUser(displayText, attachments, sessionId, promptText);
  transcript.startStreaming(sessionId, promptId);

  const sending = input.coding
    ? send(sessionId, promptText, attachments, displayText, promptId, input.coding)
    : send(sessionId, promptText, attachments, displayText, promptId);
  void sending.catch((error) => {
    if (isAgentPromptSettled(sessionId, promptId)) return;
    const latest = useSessionStore.getState();
    const detail = friendlyError(error);
    latest.markComplete({
      sessionId,
      promptId,
      stopReason: "error",
      agentResult: detail,
    });
    if (previousControl) {
      latest.restoreControl(sessionId, previousControl);
    }
    // Error banners belong to the focused conversation. A late failure from a
    // background turn must not stop or overwrite whichever session is active.
    if (latest.sessionId === sessionId) {
      latest.setError(detail, { sessionId, promptId });
    }
    useSessionsStore.getState().upsert({
      sessionId,
      status: previousControl ? sessionControlStatus(previousControl) : "failed",
    });
    input.onRejected?.(error, promptId);
    const current = useSessionStore.getState().transcripts[sessionId];
    if (!current?.streamingMessageId && !current?.pendingSendNowPromptId) {
      useMessageQueueStore.getState().pauseQueued(sessionId);
    }
  });
  return true;
}
