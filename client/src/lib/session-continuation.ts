import type { ChatMessage } from "@/stores/session-store";
import type { ContinuationTarget, SessionContinuation } from "./types";
import { inferTurnOrigin, validPromptIndex } from "./execution-turn";

export function latestContinuationTarget(messages: ChatMessage[]): ContinuationTarget | undefined {
  // Default continuation keeps the latest admitted request, including failed
  // or tool-only turns. Message-level entry points may require a public answer.
  const latest = messages[messages.length - 1];
  if (!latest) return;
  const user = [...messages].reverse().find(message => message.role === "user");
  const sameHumanTurn = latest.role === "user" || (
    (latest.execution?.origin ?? inferTurnOrigin(latest.promptId).origin) === "user"
    && (!latest.promptId || !user?.promptId || latest.promptId === user.promptId)
  );
  const index = latest.promptIndex ?? (sameHumanTurn ? user?.promptIndex : undefined);
  // Let Runtime resolve its latest checkpoint when an older transcript lacks
  // this boundary; selecting an earlier visible answer would discard context.
  if (!validPromptIndex(index)) return;
  return { promptIndex: index, promptId: latest.promptId ?? (sameHumanTurn ? user?.promptId : undefined) };
}

/** Branch at a persisted, complete turn, never between commentary/tool chunks. */
export function continuationTargets(messages: ChatMessage[]): Map<string, ContinuationTarget> {
  const targets = new Map<string, ContinuationTarget>();
  let user: ChatMessage | undefined;
  const turns = new Map<string, { answer: ChatMessage; target: ContinuationTarget }>();
  for (const message of messages) {
    if (message.role === "user") { user = message; continue; }
    const origin = message.execution?.origin ?? inferTurnOrigin(message.promptId).origin;
    // Never borrow a human turn's index for an internal wake. Old transcripts
    // without a checkpoint keep the earlier valid history entry available.
    const sameHumanTurn = origin === "user" && (!message.promptId || !user?.promptId || message.promptId === user.promptId);
    const index = message.promptIndex ?? (sameHumanTurn ? user?.promptIndex : undefined);
    if (!validPromptIndex(index)) continue;
    const promptId = message.promptId ?? (sameHumanTurn ? user?.promptId : undefined);
    const key = promptId ?? `index:${index}`;
    turns.set(key, { answer: message, target: { promptIndex: index, promptId } });
  }
  for (const { answer, target } of turns.values()) {
    if (answer.complete && (!answer.replayed || answer.stopReason)
      && answer.parts.some(part => part.kind === "text" && part.phase !== "commentary" && part.text.trim())) {
      targets.set(answer.id, target);
    }
  }
  return targets;
}

/** Missing prompt ids on old transcripts fall back to their saved turn count. */
export function inheritedMessageIds(messages: ChatMessage[], origin?: SessionContinuation): Set<string> {
  const ids = new Set<string>();
  if (!origin) return ids;
  let turn = 0;
  let inherited = true;
  let boundarySeen = false;
  for (const message of messages) {
    if (message.role === "user") turn += 1;
    const nativeIndex = message.promptIndex;
    if (validPromptIndex(nativeIndex)) inherited = nativeIndex <= origin.promptIndex;
    else if (message.role === "user") inherited = !boundarySeen && turn <= origin.inheritedTurnCount;
    if (inherited) ids.add(message.id);
    // Native turn counts include hidden wakes. Once the source boundary was
    // seen, a new optimistic human message cannot be part of copied history.
    if (validPromptIndex(nativeIndex) && nativeIndex >= origin.promptIndex
      || origin.promptId && message.promptId === origin.promptId) boundarySeen = true;
  }
  return ids;
}

export function continuationAnchor(messages: ChatMessage[], target: ContinuationTarget): string | undefined {
  if (target.promptId) {
    const native = messages.filter(message => message.promptId === target.promptId);
    if (native.some(message => validPromptIndex(message.promptIndex) && message.promptIndex !== target.promptIndex)) return;
    if (native.length) return native[native.length - 1].id;
    return;
  }
  const userIndex = messages.findIndex(message => message.role === "user" && message.promptIndex === target.promptIndex);
  if (userIndex < 0) return;
  let anchor = messages[userIndex].id;
  for (let i = userIndex + 1; i < messages.length && messages[i].role !== "user"; i++) {
    if (messages[i].promptIndex !== undefined && messages[i].promptIndex !== target.promptIndex
      || inferTurnOrigin(messages[i].promptId).origin !== "user") break;
    anchor = messages[i].id;
  }
  return anchor;
}
