import type { ToolCallContent } from "./types";

const MAX_KNOWLEDGE_OUTPUT_CHARS = 1_000_000;
const MAX_KNOWLEDGE_UI_RESULT_CHARS = 65_536;

export function knowledgeToolSource(name: unknown): "personal" | "organization" | null {
  if (typeof name !== "string") return null;
  const tool = name.trim().replace(/^echoagent_organization_memory__/, "");
  if (/^local_knowledge_(search|fetch)$/.test(tool)) return "personal";
  if (/^knowledge_(context|ask|feedback|search|fetch_document|fetch_doc|list_documents|list_docs|who_knows|submit)$/.test(tool)) return "organization";
  return null;
}

/** Native MCP completion events carry their output outside ACP content. Keep
 * the bounded text intact so citations survive live updates and history replay. */
export function knowledgeToolContent(raw: unknown): ToolCallContent[] | undefined {
  if (!raw || typeof raw !== "object") return undefined;
  const result = raw as Record<string, unknown>;
  if (result.type !== "MCP" || result.server_name !== "echoagent_organization_memory") return undefined;
  const source = knowledgeToolSource(result.tool_name);
  if (!source) return undefined;
  const label = source === "personal" ? "个人知识库" : "组织知识";
  const output = result.output;
  if (!output || typeof output !== "object") return undefined;
  const details = output as Record<string, unknown>;
  const failure = details.Error;
  if (result.is_error === true || typeof failure === "string") return [{ type: "text",
    text: `${label}操作未完成：${typeof failure === "string" ? failure.slice(0, 4000)
      : source === "personal" ? "请检查本地知识目录和文件访问权限后重试。" : "请检查组织连接后重试。"}` }];
  // Captured before the model-facing MCP text is capped; survives ACP replay.
  if (result.ui_result && typeof result.ui_result === "object" && !Array.isArray(result.ui_result)) {
    const text = JSON.stringify(result.ui_result);
    if (text.length <= MAX_KNOWLEDGE_UI_RESULT_CHARS) return [{ type: "text", text }];
  }
  const text = details.OkayOutput;
  if (typeof text !== "string") return undefined;
  if (text.length > MAX_KNOWLEDGE_OUTPUT_CHARS || /\n\n\[MCP output truncated:/.test(text)) return [{ type: "text",
    text: `${label}结果过大，未能完整展示，请缩小查询范围后重试。` }];
  return [{ type: "text", text }];
}
