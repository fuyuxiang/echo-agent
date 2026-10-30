import type { MessagePart, ToolCallView } from "@/stores/session-store";

/** A bounded view of real events, never an inferred plan or progress percentage. */
export function executionProcessPreview(parts: MessagePart[], focusedToolCallId?: string) {
  const tools: ToolCallView[] = [];
  let latestText: Extract<MessagePart, { kind: "text" | "thought" }> | undefined;
  for (const part of parts) {
    if (part.kind === "tool_call") tools.push(part.toolCall);
    else if (part.text.trim()) latestText = part;
  }

  // Keep older parallel work visible even when newer, shorter calls finish.
  const running = tools.filter(tool => tool.status === "in_progress");
  // A keyboard target must not disappear when it completes or new work arrives.
  const selected = new Set<string>();
  if (focusedToolCallId && tools.some(tool => tool.toolCallId === focusedToolCallId)) {
    selected.add(focusedToolCallId);
  }
  for (let index = running.length - 1; index >= 0 && selected.size < 3; index--) {
    selected.add(running[index].toolCallId);
  }
  for (let index = tools.length - 1; index >= 0 && selected.size < 3; index--) {
    selected.add(tools[index].toolCallId);
  }
  const visibleTools = tools.filter(tool => selected.has(tool.toolCallId));
  return {
    tools: visibleTools,
    hiddenToolCount: tools.length - visibleTools.length,
    hiddenRunningCount: running.filter(tool => !selected.has(tool.toolCallId)).length,
    excerpt: latestText ? previewExcerpt(latestText.text) : "",
    excerptKind: latestText?.kind,
  };
}

function previewExcerpt(text: string): string {
  // Do not mount another streaming Markdown tree for this two-line preview.
  // Use the latest paragraph so long thoughts still show newly received text.
  const paragraphs = readableTextTail(text)
    .split(/\n\s*\n/)
    .map(value => value.trim())
    .filter(Boolean);
  const plain = (paragraphs[paragraphs.length - 1] ?? "")
    .replace(/!\[[^\]]*\]\([^)]*\)/g, "")
    .replace(/\[([^\]]+)\]\([^)]*\)/g, "$1")
    .replace(/^\s{0,3}(?:#{1,6}\s+|>\s*|[-*+]\s+)/gm, "")
    .replace(/[*_`]/g, "")
    .replace(/\s+/g, " ")
    .trim();
  const characters = Array.from(plain);
  return characters.length > 200 ? `…${characters.slice(-200).join("")}` : plain;
}

/** Scan fence boundaries before truncating, keeping only a bounded prose tail. */
function readableTextTail(text: string): string {
  let tail = "";
  let cursor = 0;
  let fence: { marker: string; length: number } | undefined;
  const append = (value: string) => { tail = (tail + value.slice(-2_048)).slice(-2_048); };
  for (const match of text.matchAll(/^ {0,3}(`{3,}|~{3,})([^\r\n]*)(?:\r?\n|$)/gm)) {
    const marker = match[1][0];
    if (!fence) {
      // A backtick fence's info string cannot itself contain backticks.
      if (marker === "`" && match[2].includes("`")) continue;
      append(text.slice(cursor, match.index));
      append("\n\n");
      fence = { marker, length: match[1].length };
    } else if (marker === fence.marker && match[1].length >= fence.length && !match[2].trim()) {
      fence = undefined;
      cursor = match.index + match[0].length;
    }
  }
  if (!fence) append(text.slice(cursor));
  return tail;
}
