import type { ChatMessage } from "@/stores/session-store";
import type { ContinuationTarget, SessionContinuation } from "./types";

export function latestContinuationTarget(messages: ChatMessage[]): ContinuationTarget | undefined {
  const user = [...messages].reverse().find(message => message.role === "user");
  if (user?.promptIndex === undefined) return;
  return {promptIndex:user.promptIndex, promptId:user.promptId ?? messages[messages.length - 1]?.promptId};
}

/** Branch at a persisted, complete turn, never between commentary/tool chunks. */
export function continuationTargets(messages: ChatMessage[]): Map<string, ContinuationTarget> {
  const targets = new Map<string, ContinuationTarget>();
  let user: ChatMessage | undefined;
  let answer: ChatMessage | undefined;
  const finish = () => {
    if (user?.promptIndex !== undefined && Number.isSafeInteger(user.promptIndex) && user.promptIndex >= 0
      && answer?.complete && answer.parts.some(part => part.kind === "text" && part.phase !== "commentary" && part.text.trim())) {
      targets.set(answer.id, { promptIndex:user.promptIndex, promptId:user.promptId ?? answer.promptId });
    }
  };
  for (const message of messages) {
    if (message.role === "user") { finish(); user = message; answer = undefined; }
    else answer = message;
  }
  finish();
  return targets;
}

/** Missing prompt ids on old transcripts fall back to their saved turn count. */
export function inheritedMessageIds(messages: ChatMessage[], origin?: SessionContinuation): Set<string> {
  const ids = new Set<string>();
  if (!origin) return ids;
  let turn = 0;
  let inherited = true;
  for (const message of messages) {
    if (message.role === "user") {
      turn += 1;
      inherited = message.promptIndex !== undefined ? message.promptIndex <= origin.promptIndex : turn <= origin.inheritedTurnCount;
    }
    if (inherited) ids.add(message.id);
  }
  return ids;
}

export function continuationAnchor(messages: ChatMessage[], target: ContinuationTarget): string | undefined {
  let userIndex = messages.findIndex(message => message.role === "user"
    && (target.promptId ? message.promptId === target.promptId : message.promptIndex === target.promptIndex));
  // Some historical replays put prompt ids on the assistant only.
  if (userIndex < 0 && target.promptId) {
    const answer = messages.findIndex(message => message.promptId === target.promptId);
    if (answer >= 0) { userIndex = answer; while (userIndex > 0 && messages[userIndex].role !== "user") userIndex--; }
  }
  if (userIndex < 0) return;
  let anchor = messages[userIndex].id;
  for (let i = userIndex + 1; i < messages.length && messages[i].role !== "user"; i++) anchor = messages[i].id;
  return anchor;
}
