import type { PermissionRequest } from "./types";
import { detectToolRenderer } from "./tool-renderers";

function record(value: unknown): Record<string, unknown> | undefined {
  return value && typeof value === "object" && !Array.isArray(value)
    ? value as Record<string, unknown> : undefined;
}

function text(value: unknown): string | undefined {
  return typeof value === "string" && value.trim() ? value : undefined;
}

/** Display the proposed operation once, without inferring shell effects or risk. */
export function permissionPresentation(request: PermissionRequest) {
  const raw = record(request.rawInput);
  const wrapped = raw?.variant === "UseTool" && record(raw.tool_input);
  const input = wrapped || raw;
  const kind = (wrapped && text(raw?.tool_name) || request.toolKind).toLowerCase();
  const renderer = detectToolRenderer(kind);
  const terminal = renderer === "command";
  const file = renderer === "read" || renderer === "edit" || kind === "delete";
  const web = renderer === "search" || renderer === "weather";
  const sourceTitle = request.title.trim();
  const commandFromTitle = terminal ? sourceTitle.match(/^(?:Execute|Run)\s+`([\s\S]+)`[.!。]?$/i)?.[1] : undefined;
  const previewEntry = ["command", "cmd", "path", "file_path", "filePath", "url", "query"]
    .map(key => [key, input?.[key]] as const).find(([, value]) => text(value));
  const previewKeys = previewEntry && (["command", "cmd"].includes(previewEntry[0]) ? ["command", "cmd"]
    : ["path", "file_path", "filePath"].includes(previewEntry[0]) ? ["path", "file_path", "filePath"] : [previewEntry[0]]);
  let preview = text(previewEntry?.[1]) ?? commandFromTitle ?? text(request.rawInput);
  const truncatedCommandTitle = terminal && preview && sourceTitle.match(/^(?:Execute|Run)\s+`([\s\S]+)\n\[truncated\]$/i)?.[1];
  const titleIsCommandPreview = Boolean(truncatedCommandTitle && preview?.startsWith(truncatedCommandTitle));
  const previewKind = terminal && (!previewEntry || ["command", "cmd"].includes(previewEntry[0])) ? "命令" : "内容";
  const readableTitle = /[\u3400-\u9fff]/u.test(sourceTitle) && sourceTitle.length <= 100
    && !/[\n`{}]/.test(sourceTitle) && !/^(?:execute|run|read|write|edit|delete|fetch|search)\b/i.test(sourceTitle)
    && (!preview || !sourceTitle.includes(preview));
  const title = sourceTitle === "conversation_history" ? "是否允许读取会话记录？"
    : readableTitle ? sourceTitle
      : terminal ? "是否允许运行这条命令？"
        : kind === "delete" ? "是否允许删除此文件？"
          : renderer === "edit" ? "是否允许修改此文件？"
            : renderer === "read" ? "是否允许读取以下内容？"
              : web ? "是否允许执行此网络操作？" : "是否允许执行此操作？";
  const description = terminal ? text(input?.description) : undefined;
  const purpose = description && description.length <= 200 && description.trim() !== title
    && description !== preview ? description.trim() : undefined;

  // Only remove values already visible elsewhere. Preserve all other arguments,
  // including alternative command fields, working directories and file content.
  const remaining = input ? Object.fromEntries(Object.entries(input).filter(([key, value]) =>
    !(previewKeys?.includes(key) && value === preview)
    && !(key === "description" && purpose && value === description))) : undefined;
  let details: unknown = wrapped ? { ...raw, tool_input: remaining && Object.keys(remaining).length ? remaining : undefined }
    : remaining && Object.keys(remaining).length ? remaining : undefined;
  if (request.rawInput != null && !raw && !text(request.rawInput)) details = request.rawInput;

  if (sourceTitle && sourceTitle !== title && sourceTitle !== "conversation_history" && !titleIsCommandPreview) {
    const remainder = preview ? sourceTitle.split(preview).join("") : sourceTitle;
    if (remainder.replace(/^(?:execute|run)\s*/i, "").replace(/[`'".!。\s]/g, "")) {
      const original = preview ? sourceTitle.split(preview).join("[见上方操作内容]") : sourceTitle;
      if (!preview && details == null) preview = sourceTitle;
      else details = { 原始请求: original, ...(record(details) ?? (details != null ? { 参数: details } : {})) };
    }
  }
  return { category: terminal ? "terminal" : file ? "file" : web ? "web" : "tool",
    label: terminal ? "终端" : file ? "文件" : web ? "网络" : "工具",
    title, purpose, preview, previewKind, details };
}
