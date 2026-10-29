import type { ChatMessage } from "@/stores/session-store";
import type { QuestionHistoryItem } from "@/components/QuestionHistoryPopover";
import { partitionAssistantParts, summarizeExecutionProcess } from "./execution-process";
import { getExecutionNotice } from "./execution-notice";

export interface ConversationTurn extends QuestionHistoryItem {
  answer: string;
  status: string;
}

/** Public answers only: reasoning, tool input/output and injected context never enter previews. */
export function buildConversationTurns(messages: ChatMessage[], questions: QuestionHistoryItem[]): ConversationTurn[] {
  const byId = new Map(questions.map(item => [item.id, item]));
  const turns: ConversationTurn[] = [];
  let current: ConversationTurn | undefined;
  for (const message of messages) {
    if (message.role === "user") {
      const question = byId.get(message.id);
      current = question ? { ...question, answer: "", status: "等待回复" } : undefined;
      if (current) turns.push(current);
    } else if (message.role === "assistant" && current) {
      const groups = partitionAssistantParts(message.parts);
      current.status = message.complete
        ? getExecutionNotice(message)?.title ?? (groups.responseParts.some(part => part.text.trim()) ? "已回复" : "本轮已结束")
        : summarizeExecutionProcess(groups.processParts, true).title;
      if (message.complete) {
        const answer = groups.responseParts.map(part => part.text).join(" ")
          .replace(/!\[[^\]]*\]\([^)]*\)/g, "[图片]")
          .replace(/\[([^\]]+)\]\([^)]*\)/g, "$1")
          .replace(/[#*_`>]+/g, "").replace(/\s+/g, " ").trim();
        if (answer) current.answer = answer.slice(0, 180);
      }
    }
  }
  return turns;
}

/** The reading turn is the interval from its prompt to the next prompt. */
export function readingTurnIndex(tops: number[], anchor: number): number {
  let index = 0;
  for (let i = 0; i < tops.length; i++) {
    if (tops[i] > anchor) break;
    index = i;
  }
  return index;
}
