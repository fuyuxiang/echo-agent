import type { PersonalMemoryInput } from "./agent-client";
import { memoryTitleFromContent } from "./memory-title";

/** Import is a reviewable draft, never an instruction to execute. */
export function parseMemoryImport(text: string): PersonalMemoryInput[] {
  if (text.length > 200_000) throw new Error("导入内容过长，请分批导入");
  let values: unknown[];
  const trimmed = text.trim().replace(/^```(?:json)?\s*\n/, "").replace(/\n```$/, "");
  if (/^[\[{]/.test(trimmed)) {
    let parsed: unknown;
    try { parsed = JSON.parse(trimmed); } catch { throw new Error("JSON 格式不正确，请检查或改用每行一条的文本"); }
    values = Array.isArray(parsed) ? parsed : typeof parsed === "object" && parsed !== null && "memories" in parsed && Array.isArray(parsed.memories) ? parsed.memories : [];
  } else {
    values = trimmed.split(/\n+/).map((line) => line.replace(/^\s*(?:[-*•]|\d+[.、])\s*/, "").trim()).filter((line) => line && !/^#+\s/.test(line));
  }
  if (values.length > 100) throw new Error("一次最多预览 100 条记忆，请分批导入");
  const seen = new Set<string>();
  return values.flatMap((value): PersonalMemoryInput[] => {
    const item = typeof value === "string" ? { content: value } : value as Record<string, unknown> | null;
    if (!item || typeof item.content !== "string" || !item.content.trim()) return [];
    if ("status" in item && item.status !== "active") return [];
    const content = item.content.trim();
    if (content.length > 4000) throw new Error("单条记忆最多 4000 字，请拆分后导入");
    if (seen.has(content)) return [];
    seen.add(content);
    const kind = "kind" in item && ["user", "feedback", "project", "reference", "task"].includes(String(item.kind)) ? item.kind as PersonalMemoryInput["kind"] : "user";
    return [{ scope: "scope" in item && item.scope === "workspace" ? "workspace" : "global", kind,
      key: "key" in item && typeof item.key === "string" ? [...item.key.trim()].slice(0, 160).join("") || memoryTitleFromContent(content) : memoryTitleFromContent(content), content,
      pinned: true, validUntil: "validUntil" in item && typeof item.validUntil === "number" && Number.isFinite(item.validUntil) ? item.validUntil : null }];
  });
}
