import { invoke } from "@tauri-apps/api/core";
import type { ToolCallView } from "@/stores/session-store";

export type OfficeFormat = "docx" | "pdf" | "xlsx" | "pptx";

export interface DocumentExportReceipt {
  path: string;
  format: OfficeFormat;
  byteSize: number;
  sha256: string;
  editableSourceWarning?: string;
}

const OFFICE_CREATE_TOOL = "echoagent-office__office_create";
const OFFICE_UPDATE_TOOL = "echoagent-office__office_document_update";

function recordInput(value: unknown): Record<string, unknown> | null {
  return value && typeof value === "object" && !Array.isArray(value)
    ? value as Record<string, unknown> : null;
}

export function isOfficeCreateTool(toolCall: ToolCallView): boolean {
  return toolCall.officeReceipt != null
    || [OFFICE_CREATE_TOOL, OFFICE_UPDATE_TOOL].includes(String(recordInput(toolCall.rawInput)?.tool_name))
    || /echoagent-office__(?:office_create|office_document_update)/i.test(`${toolCall.kind} ${toolCall.title}`);
}

/** Runtime discovery wraps an Office call in use_tool; direct MCP calls stay flat. */
export function officeInputFromToolCall(toolCall: ToolCallView): Record<string, unknown> | null {
  if (!isOfficeCreateTool(toolCall)) return null;
  const input = recordInput(toolCall.rawInput);
  return [OFFICE_CREATE_TOOL, OFFICE_UPDATE_TOOL].includes(String(input?.tool_name)) ? recordInput(input?.tool_input) : input;
}

function validReceipt(value: unknown): DocumentExportReceipt | null {
  const receipt = recordInput(value);
  if (!receipt) return null;
  const { path, format, byteSize, sha256, editableSourceWarning } = receipt;
  if (typeof path !== "string"
    || (format !== "docx" && format !== "pdf" && format !== "xlsx" && format !== "pptx")
    || typeof byteSize !== "number" || byteSize <= 0
    || typeof sha256 !== "string" || !/^[a-f0-9]{64}$/i.test(sha256)) return null;
  return {
    path, format, byteSize, sha256,
    ...(format === "docx" && typeof editableSourceWarning === "string" && editableSourceWarning.trim()
      ? { editableSourceWarning } : {}),
  };
}

/** UI and artifact catalog use only receipts validated from Runtime rawOutput. */
export function officeReceiptFromToolCall(toolCall: ToolCallView): DocumentExportReceipt | null {
  if (!isOfficeCreateTool(toolCall) || toolCall.status !== "completed") return null;
  return validReceipt(toolCall.officeReceipt);
}

/** Runtime publishes MCP results in ACP rawOutput, without content blocks. */
export function officeReceiptFromRawOutput(raw: unknown): DocumentExportReceipt | null {
  if (!raw || typeof raw !== "object") return null;
  const output = raw as Record<string, unknown>;
  if (output.type !== "MCP"
    || output.is_error === true
    || output.server_name !== "echoagent-office"
    || (output.tool_name !== "office_create"
      && output.tool_name !== "echoagent-office__office_create"
      && output.tool_name !== "office_document_update"
      && output.tool_name !== OFFICE_UPDATE_TOOL)) return null;
  const details = output.output;
  if (!details || typeof details !== "object") return null;
  const text = (details as Record<string, unknown>).OkayOutput;
  if (typeof text !== "string" || text.length > 4096) return null;
  try { return validReceipt(JSON.parse(text)); } catch { return null; }
}

export function exportOfficeDocument(
  title: string,
  markdown: string,
  format: OfficeFormat,
  sessionId?: string,
): Promise<DocumentExportReceipt | null> {
  return invoke("document_export", { request: { title, markdown, format }, sessionId: sessionId ?? null });
}
