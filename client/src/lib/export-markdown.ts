/**
 * 会话导出为 Markdown — 从 session-store 的 messages 构建一份可读的
 * Markdown 文档（用户问题 / 助手回答 / 思考过程 / 工具调用摘要）。
 *
 * 对齐 EchoAgent 的"导出对话"功能。
 */
import type { ChatMessage } from "@/stores/session-store";
import { partitionAssistantParts } from "@/lib/execution-process";

export interface SessionExportOptions {
  /** Include reasoning and tool records. Defaults to public responses only. */
  includeProcess?: boolean;
}

/** Build a Markdown document from a session's message list. */
export function buildSessionMarkdown(
  messages: ChatMessage[],
  title?: string,
  options: SessionExportOptions = {},
): string {
  const lines: string[] = [];
  lines.push(`# ${title || "对话导出"}`);
  lines.push("");
  lines.push(`> 导出于 ${new Date().toLocaleString()}`);
  lines.push("");
  lines.push("---");
  lines.push("");

  for (const m of messages) {
    if (m.role === "user") {
      const text = m.parts
        .filter((p) => p.kind === "text")
        .map((p) => p.text)
        .join("\n");
      if (!text.trim()) continue;
      lines.push("## 🧑 用户");
      lines.push("");
      lines.push(text);
      lines.push("");
    } else {
      const groups = partitionAssistantParts(m.parts);
      const responseText = groups.responseParts
        .map((part) => part.text)
        .filter((text) => text.trim());
      if (
        responseText.length === 0
        && (!options.includeProcess || groups.processParts.length === 0)
      ) continue;

      lines.push("## 🤖 EchoAgent");
      lines.push("");

      if (options.includeProcess && groups.processParts.length > 0) {
        lines.push("### 执行过程");
        lines.push("");
        appendProcessMarkdown(lines, groups.processParts);
      }

      if (responseText.length > 0) {
        lines.push(responseText.join("\n\n"));
        lines.push("");
      }
    }

    lines.push("---");
    lines.push("");
  }

  return lines.join("\n");
}

/** Keep exported process records in the same order the user saw in the UI. */
function appendProcessMarkdown(lines: string[], parts: ChatMessage["parts"]): void {
  let inToolList = false;
  for (const part of parts) {
    if (part.kind === "tool_call") {
      if (!inToolList) {
        lines.push("**操作记录：**");
        lines.push("");
        inToolList = true;
      }
      const tool = part.toolCall;
      const icon = tool.status === "completed" ? "✅" : tool.status === "failed" ? "❌" : "⏳";
      lines.push(`- ${icon} \`${tool.kind}\` — ${tool.title}`);
      continue;
    }

    if (inToolList) lines.push("");
    inToolList = false;
    lines.push(part.kind === "text" ? "> **阶段进展**" : "> **分析详情**");
    lines.push(">");
    for (const line of part.text.split("\n")) lines.push(`> ${line}`);
    lines.push("");
  }
  if (inToolList) lines.push("");
}

/** Sanitize a string for use as a filename. */
export function sanitizeFilename(name: string): string {
  return name
    .replace(/[<>:"/\\|?*\x00-\x1f]/g, "_")
    .replace(/\s+/g, "_")
    .slice(0, 80)
    .replace(/^[._]+/, "") || "对话导出";
}
