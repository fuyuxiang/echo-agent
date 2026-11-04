import type { ChatMessage } from "@/stores/session-store";
import { extractPlainText } from "@/lib/extract-text";
import { isExecutionSettled } from "@/lib/execution-process";
import { terminalSessionStatus } from "@/lib/turn-status";

export interface CodingPlanResult {
  status: "none" | "generating" | "complete" | "interrupted";
  text: string;
}

/** Only a response to the latest explicit Plan turn can seed its editable draft. */
export function codingPlanResult(messages: ChatMessage[]): CodingPlanResult {
  let promptIndex = messages.length - 1;
  while (promptIndex >= 0 && messages[promptIndex].role !== "user") promptIndex--;
  if (promptIndex < 0) return { status: "none", text: "" };
  const prompt = messages[promptIndex];
  if (!/^\[回声代码 · plan(?: · 继续当前会话)?\](?:\r?\n|$)/.test(prompt.agentText ?? extractPlainText(prompt))) return { status: "none", text: "" };
  const response = messages.slice(promptIndex + 1).reverse().find(message => message.role === "assistant");
  if (!response?.complete) return { status: "generating", text: "" };
  // complete also closes cancelled/error bubbles and truncated replay tails.
  // Older live bridges can omit a stop reason; replay requires a terminal ACK.
  if (!isExecutionSettled(response)
    || terminalSessionStatus({ ...response, stopReason: response.stopReason ?? "end_turn" }) !== "completed") {
    return { status: "interrupted", text: "" };
  }
  return { status: "complete", text: extractPlainText(response) };
}
