import type { ChatMessage, ToolCallView } from "@/stores/session-store";
import { partitionAssistantParts } from "./execution-process";
import { findToolRecoveries } from "./tool-recovery";
import { isModelQuotaExhausted } from "./error-format";

export interface ExecutionNotice {
  kind: "runtime" | "delivery" | "stopped" | "empty" | "environment";
  tone: "error" | "warning" | "neutral";
  title: string;
  description: string;
  tools: ToolCallView[];
  modelSettings?: boolean;
  securitySettings?: boolean;
}

type Delivery = "file" | "send" | "upload" | "publish";

/** Only explicit tool identities establish an action. Never infer effects from
 * a command, display title, model prose, or arbitrary error/output text. */
function toolIdentity(tool: ToolCallView): string {
  const input = tool.rawInput && typeof tool.rawInput === "object" && !Array.isArray(tool.rawInput)
    ? tool.rawInput as Record<string, unknown> : undefined;
  const identity = typeof input?.tool_name === "string" ? input.tool_name : tool.toolName || tool.kind;
  return identity.split(/__|\./).pop()?.toLowerCase() ?? "";
}

function deliveryKind(tool: ToolCallView): Delivery | undefined {
  const name = toolIdentity(tool);
  if (/^(write|edit|write_file|edit_file|multi_edit|apply_patch|office_create|create_document|export_document)$/.test(name ?? "")) return "file";
  if (/^(send_message|post_message|send_email|send_mail|notify|email)$/.test(name ?? "")) return "send";
  if (/^(upload|upload_file|upload_document|browser_upload)$/.test(name ?? "")) return "upload";
  if (/^(publish|publish_site|publish_document|deploy)$/.test(name ?? "")) return "publish";
  return undefined;
}

const labels: Record<Delivery, string> = { file: "文件操作", send: "消息发送", upload: "文件上传", publish: "发布操作" };

export function getExecutionNotice(message: ChatMessage, active = false, runtimeError?: string): ExecutionNotice | null {
  if (message.role !== "assistant" || active || !message.complete || message.cancelTrigger === "send_now") return null;
  const recovered = new Set(findToolRecoveries(message.parts).map((item) => item.failedToolCallId));
  const tools = message.parts.flatMap((part) => part.kind === "tool_call" ? [part.toolCall] : []);
  const affected = tools.filter((tool) => deliveryKind(tool) && tool.status !== "completed" && !recovered.has(tool.toolCallId));
  const historyIncomplete = message.cancellationCategory === "session_replay_incomplete";
  const uncertain = historyIncomplete || affected.some((tool) => tool.status === "in_progress");
  const effects = [...new Set(affected.map((tool) => labels[deliveryKind(tool)!]))].join("、");
  const impact = affected.length ? `${effects}${uncertain ? "的结果尚未确认，请先检查已有结果，避免重复操作。" : "未成功，请检查目标结果后再继续。"}` : "已有内容已保留。";
  const reason = message.stopReason;
  const category = message.cancellationCategory;
  const failed = ["error", "rate_limit", "rate_limited", "refusal", "content_filter", "max_tokens", "max_turns"].includes(reason ?? "")
    || ["HookDenied", "max_turns_reached", "action_stationarity"].includes(category ?? "");
  const blockedComputerTools = tools.filter(tool => tool.status === "failed" && toolIdentity(tool).startsWith("computer_") && !recovered.has(tool.toolCallId));
  // Only actual failed desktop operations or a confirmed runtime error can
  // request setup. Passive capability probes and model prose add no UI.
  const environmentDetail = [failed ? `${runtimeError ?? ""} ${message.agentResult ?? ""}` : "",
    ...blockedComputerTools.flatMap(tool => tool.content.flatMap(content => content.type === "text" ? [content.text] : []))].join("\n");
  const permissionMissing = /(?:尚未授予|需要授予|需要在[^\n]{0,80}授予|请检查)[^\n]{0,80}(?:屏幕录制|辅助功能)(?:权限)?|(?:screen recording|accessibility) permission (?:required|denied|not granted)/i.test(environmentDetail);
  const desktopUnavailable = /桌面暂不可用|无法打开 Windows 交互桌面|desktop (?:is )?unavailable/i.test(environmentDetail);
  if (permissionMissing || desktopUnavailable) return {
    kind: "environment", tone: "warning", title: permissionMissing ? "电脑操作需要系统权限" : "桌面暂不可用",
    description: (affected.length ? impact : "已有内容已保留。") + (permissionMissing ? "请在设置中授予所需权限，完成后发送新消息继续。"
      : "请解锁电脑并回到可操作的桌面，再发送新消息继续。"),
    tools: blockedComputerTools, ...(permissionMissing ? { securitySettings: true } : {}),
  };
  if (failed) {
    // These strings only refine an already confirmed runtime failure, never its
    // success state. Detailed native errors remain in the process disclosure.
    const detail = `${runtimeError ?? ""} ${message.agentResult ?? ""}`.toLowerCase();
    const quota = isModelQuotaExhausted(detail);
    const connection = /disconnect|connection|network|timeout|timed out|连接|网络|超时/.test(detail);
    const title = quota ? "模型额度已用尽" : ["rate_limit", "rate_limited"].includes(reason ?? "") ? "模型请求受限"
      : category === "HookDenied" ? "操作被安全规则阻止"
      : reason === "max_tokens" ? "回复达到长度上限"
      : reason === "max_turns" || category === "max_turns_reached" ? "本轮已达到执行轮数上限"
      : category === "action_stationarity" ? "本轮因持续无进展而停止"
      : reason === "refusal" || reason === "content_filter" ? "模型未能处理本次请求"
      : connection ? "连接中断，本轮未能继续" : "本轮未能继续";
    return { kind: "runtime", tone: "error", title, description: impact, tools: affected,
      ...(quota ? { modelSettings: true } : {}) };
  }
  if (affected.length) return {
    kind: "delivery", tone: "warning", title: uncertain ? "有操作的结果尚未确认" : "有操作未成功",
    description: impact, tools: affected,
  };
  // Missing replay receipts alone do not establish a failure or a success.
  // Keep diagnostic uncertainty inside the disclosure unless a delivery action
  // requires the user's attention above.
  if (historyIncomplete) return null;
  if (reason === "cancelled") return {
    kind: "stopped", tone: "neutral",
    title: category === "PermissionRejected" ? "未获授权，执行已停止" : category === "PermissionCancelled" ? "授权已取消，执行已停止" : "执行已停止",
    description: "已有内容已保留，可发送新消息继续。", tools: [],
  };
  const hasAnswer = partitionAssistantParts(message.parts).responseParts.some((part) => part.text.trim());
  if (!hasAnswer && reason === "end_turn" && !tools.some((tool) => tool.status === "completed")) return {
    kind: "empty", tone: "warning", title: "本轮没有生成回复", description: "可以重新尝试，或展开过程查看原因。", tools: [],
  };
  return null;
}

/** Continue within the existing conversation. The visible follow-up uses plain
 * action names; IDs, sensitive arguments, and outputs stay in the original log. */
export function executionContinuationPrompt(notice: ExecutionNotice): string {
  const actions = [...new Set(notice.tools.map(deliveryKind).filter((kind): kind is Delivery => Boolean(kind)))].map((kind) => labels[kind]);
  return `请检查上一轮已有结果${actions.length ? `，重点核对${actions.join("、")}` : ""}，只继续尚未完成的工作。先核对文件、上传结果、消息回执或发布状态，确认是否已经成功；不要重复执行已完成的操作。对于结果不明的发送、上传或发布，无法确认时先说明情况并询问我，不要直接重新提交。`;
}
