import type { MessagePart } from "@/stores/session-store";

/** A bounded view of real events, never an inferred plan or progress percentage. */
export function executionProgressText(parts: MessagePart[]): string {
  // Public progress and internal analysis have different audiences. Never use
  // thoughts, tool output, or a final answer as the default progress summary.
  for (let index = parts.length - 1; index >= 0; index--) {
    const part = parts[index];
    if (part.kind !== "text" || part.phase === "final_answer" || part.phase === "partial_answer") continue;
    const excerpt = previewExcerpt(part.text);
    if (excerpt) return excerpt;
  }
  return "";
}

export function previewExcerpt(text: string): string {
  const paragraphs = readableProse(text, "end")
    .split(/\n\s*\n/)
    .map(plainProgressText)
    .filter(Boolean);
  // Keep the context of the current paragraph, rather than selecting a final
  // sentence that might just say "please wait". The full text stays in records.
  const characters = Array.from(paragraphs[paragraphs.length - 1] ?? "");
  return characters.length > 200 ? `…${characters.slice(-200).join("")}` : characters.join("");
}

/** A stable heading from the beginning of actual public progress. */
export function progressStageTitle(text: string): string {
  const plain = plainProgressText(readableProse(text, "start"));
  const firstSentence = plain.match(/^[^。！？!?\n]+/)?.[0]?.trim() ?? "";
  const characters = Array.from(firstSentence);
  return characters.slice(0, 48).join("") + (characters.length > 48 ? "…" : "");
}

function plainProgressText(text: string): string {
  return text
    .replace(/!\[[^\]]*\]\([^)]*\)/g, "")
    .replace(/\[([^\]]+)\]\([^)]*\)/g, "$1")
    .replace(/^\s{0,3}(?:#{1,6}\s+|>\s*|[-*+]\s+)/gm, "")
    .replace(/[*_`]/g, "")
    .replace(/\s+/g, " ")
    .trim();
}

/** Scan fence boundaries before truncating, keeping only a bounded prose tail. */
function readableProse(text: string, edge: "start" | "end"): string {
  let tail = "";
  let cursor = 0;
  let fence: { marker: string; length: number } | undefined;
  const append = (value: string) => {
    tail = edge === "start" ? (tail + value.slice(0, 2_048)).slice(0, 2_048)
      : (tail + value.slice(-2_048)).slice(-2_048);
  };
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
