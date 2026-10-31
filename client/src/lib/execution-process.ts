import type { ChatMessage, MessagePart, ToolCallView } from "@/stores/session-store";
import { detectToolCallRenderer, isToolDiscovery } from "@/lib/tool-renderers";
import { findToolRecoveries, type ToolRecovery } from "@/lib/tool-recovery";

export type TextMessagePart = Extract<MessagePart, { kind: "text" }>;

/** Closing a replay bubble only stops its spinner; it is not a terminal ACK. */
export function isExecutionSettled(message: Pick<ChatMessage,"complete" | "replayed" | "stopReason">): boolean {
  return message.complete && (!message.replayed || Boolean(message.stopReason));
}

const INTERNAL_REASONING_TAGS = ["think", "thinking", "reasoning", "analysis"] as const;
const INTERNAL_REASONING_OPENING = /^\s*<(think|thinking|reasoning|analysis)(?:\s[^>]*)?>/i;

export interface AssistantPartGroups {
  /** Reasoning, tool calls, and intermediate model commentary. */
  processParts: MessagePart[];
  /** User-facing partial/final answers, plus unclassified Runtime text. */
  responseParts: TextMessagePart[];
  /** The current generation is producing an answer, rather than calling tools. */
  answerStarted: boolean;
}

/**
 * Split one assistant turn into a compact execution process and its final answer.
 *
 * ACP AgentMessageChunk is public text. Only explicit commentary metadata can
 * move it into the process: a tool call does not prove that the preceding text
 * was merely commentary. Current Runtime and historical events can omit phase,
 * so preserve their text in the conversation, copy, search and export alike.
 * Generation ids still help avoid announcing a final answer during tool work.
 */
export function partitionAssistantParts(
  parts: MessagePart[],
  options: { organizeProgress?: boolean } = {},
): AssistantPartGroups {
  const normalizedParts = expandTaggedThinking(parts);
  const lastMeaningfulPart = [...normalizedParts].reverse().find(part =>
    part.kind === "tool_call" || part.text.trim().length > 0,
  );
  const toolStreams = new Set(normalizedParts.flatMap(part =>
    part.kind === "tool_call" && part.streamId ? [part.streamId] : []));
  const lastToolIndex = normalizedParts.reduce((last, part, index) => part.kind === "tool_call" ? index : last, -1);
  const hasRunningTools = normalizedParts.some(part => part.kind === "tool_call" && part.toolCall.status === "in_progress");
  // Presentation groups intermediate public text with the work it accompanies.
  // Keep the source untouched: copy/search/export callers can still request all
  // public text, and explicit partial/final answers always remain in the answer.
  const responseParts = normalizedParts.filter((part, index): part is TextMessagePart => {
    if (!isTextPart(part) || part.phase === "commentary") return false;
    if (!options.organizeProgress || part.phase) return true;
    return !hasRunningTools && index > lastToolIndex && !(part.streamId && toolStreams.has(part.streamId));
  });
  const responseSet = new Set<MessagePart>(responseParts);
  return {
    processParts: normalizedParts.filter(part => !responseSet.has(part)),
    responseParts,
    answerStarted: lastMeaningfulPart?.kind === "text"
      && lastMeaningfulPart.phase !== "partial_answer" && responseSet.has(lastMeaningfulPart)
      && (lastMeaningfulPart.phase === "final_answer"
        || !lastMeaningfulPart.streamId || !toolStreams.has(lastMeaningfulPart.streamId)),
  };
}

/**
 * Some OpenAI-compatible gateways return reasoning and the answer in one
 * AgentMessageChunk using a literal reasoning envelope. Convert the common
 * `<think>`, `<thinking>`, `<reasoning>`, and `<analysis>` variants into normal
 * thought/text parts so live output, replay, copy, search, and export all share
 * the same behavior. A missing closing tag is expected while streaming.
 */
function expandTaggedThinking(parts: MessagePart[]): MessagePart[] {
  return parts.flatMap((part) => {
    if (part.kind !== "text") return [part];

    const opening = part.text.match(INTERNAL_REASONING_OPENING);
    if (!opening) {
      const pendingTag = part.text.trimStart().toLowerCase();
      // Avoid briefly flashing a fragmented opening tag while tokens stream.
      if (isPendingReasoningOpening(pendingTag)) return [];
      return [part];
    }

    const tag = opening[1].toLowerCase();
    const afterOpening = part.text.slice(opening[0].length);
    const closing = new RegExp(`</${tag}\\s*>`, "i").exec(afterOpening);
    const thoughtText = (closing
      ? afterOpening.slice(0, closing.index)
      : afterOpening).trim();
    const expanded: MessagePart[] = [];
    if (thoughtText) {
      expanded.push({
        kind: "thought",
        text: thoughtText,
        ...(part.stageTitle ? { stageTitle: part.stageTitle } : {}),
        ...(part.streamId ? { streamId: part.streamId } : {}),
      });
    }
    if (closing) {
      const answerText = afterOpening
        .slice(closing.index + closing[0].length)
        .trimStart();
      if (answerText) {
        expanded.push({
          ...part,
          kind: "text",
          text: answerText,
          ...(part.streamId ? { streamId: part.streamId } : {}),
        });
      }
    }
    return expanded;
  });
}

function isPendingReasoningOpening(value: string): boolean {
  if (!value) return false;
  if (INTERNAL_REASONING_TAGS.some((tag) => `<${tag}>`.startsWith(value))) return true;
  return /^<(think|thinking|reasoning|analysis)(?:\s[^>]*)?$/i.test(value);
}

export interface ExecutionProcessSummary {
  state: "running" | "complete" | "partial" | "failed" | "attention" | "stopped";
  title: string;
  description?: string;
  toolCount: number;
  completedToolCount: number;
  failedToolCount: number;
  unfinishedToolCount: number;
  unresolvedToolCount: number;
  recoveredTools: ToolRecovery[];
  thoughtCount: number;
  changedFiles: string[];
}

/** Build the short, semantic status shown in the process header. */
export function summarizeExecutionProcess(
  parts: MessagePart[],
  active: boolean,
  stopReason?: string,
  cancellationCategory?: string,
  cancelTrigger?: string,
  options: { settled?: boolean } = {},
): ExecutionProcessSummary {
  const tools = parts
    .filter((part): part is Extract<MessagePart, { kind: "tool_call" }> => part.kind === "tool_call")
    .map((part) => part.toolCall);
  const thoughts = parts.filter((part) => part.kind === "thought");
  const failedToolCount = tools.filter((tool) => tool.status === "failed").length;
  const completedToolCount = tools.filter((tool) => tool.status === "completed").length;
  const unfinishedToolCount = tools.filter((tool) => tool.status === "in_progress").length;
  const currentTool = [...tools].reverse().find((tool) => tool.status === "in_progress");
  const recoveredTools = findToolRecoveries(parts);
  const unresolvedToolCount = failedToolCount - recoveredTools.length;
  const changedFiles = [...new Set(
    tools.filter((tool) => tool.status === "completed").flatMap(toolDiffPaths),
  )];

  const abnormalStop = [
    "error",
    "rate_limit",
    "rate_limited",
    "refusal",
    "content_filter",
    "max_tokens",
    "max_turns",
  ].includes(stopReason ?? "");
  const abnormalCancellation = [
    "HookDenied",
    "max_turns_reached",
    "action_stationarity",
  ].includes(cancellationCategory ?? "");
  let state: ExecutionProcessSummary["state"];
  let title: string;
  let description: string | undefined;
  if (active) {
    state = "running";
    title = currentTool ? activeToolLabel(currentTool) : unresolvedToolCount ? "正在继续处理" : "正在分析任务";
  } else if (abnormalStop || abnormalCancellation) {
    state = "failed";
    title = abnormalCancellation ? cancelledProcessTitle(cancellationCategory) : abnormalProcessTitle(stopReason);
    description = "本轮执行未正常结束，已保留执行记录和已有内容，可查看原因后重试。";
  } else if (cancellationCategory === "session_replay_incomplete") {
    state = "attention";
    title = "执行记录不完整";
    description = "历史记录缺少完成结果，无法确认这些操作是否完成。";
  } else if (stopReason === "cancelled") {
    state = "stopped";
    title = cancelledProcessTitle(cancellationCategory, cancelTrigger);
  } else if (options.settled === false || stopReason && stopReason !== "end_turn") {
    state = "attention";
    title = "执行状态待确认";
    description = "尚未收到可确认的完成状态，已有回复不代表执行已经完成。";
  } else if (unresolvedToolCount || unfinishedToolCount) {
    state = completedToolCount > 0 ? "partial" : unfinishedToolCount > 0 ? "attention" : "failed";
    title = state === "partial" ? "部分操作未完成" : state === "attention" ? "执行结果待确认" : "操作执行未完成";
    description = [unresolvedToolCount ? `${unresolvedToolCount} 项失败操作尚未确认恢复` : "",
      unfinishedToolCount ? `${unfinishedToolCount} 项操作未记录完成结果` : ""].filter(Boolean).join("；") + "。展开可查看具体操作和原因。";
  } else {
    state = "complete";
    title = recoveredTools.length ? "执行过程" : tools.length > 0 ? "已完成执行过程" : "已完成思考";
  }

  return {
    state,
    title,
    description,
    toolCount: tools.length,
    completedToolCount,
    failedToolCount,
    unfinishedToolCount,
    unresolvedToolCount,
    recoveredTools,
    thoughtCount: thoughts.length,
    changedFiles,
  };
}

function cancelledProcessTitle(category?: string, trigger?: string): string {
  if (trigger === "send_now") return "已切换到新请求";
  switch (category) {
    case "max_turns_reached":
      return "已达到执行轮数上限";
    case "PermissionRejected":
      return "权限未批准，执行已停止";
    case "PermissionCancelled":
      return "权限请求已取消";
    case "HookDenied":
      return "执行被安全规则阻止";
    case "action_stationarity":
      return "执行因无进展而停止";
    default:
      return "已停止执行";
  }
}

function abnormalProcessTitle(stopReason?: string): string {
  if (stopReason === "refusal" || stopReason === "content_filter") {
    return "模型未能处理本次请求";
  }
  if (stopReason === "max_tokens") return "回复达到长度上限";
  if (stopReason === "max_turns") return "已达到执行轮数上限";
  return "执行未正常完成";
}

export function formatProcessDuration(milliseconds: number): string {
  const seconds = Math.max(0, Math.floor(milliseconds / 1_000));
  if (seconds < 60) return `${seconds}秒`;
  const minutes = Math.floor(seconds / 60);
  const remaining = seconds % 60;
  if (minutes < 60) return remaining > 0 ? `${minutes}分${remaining}秒` : `${minutes}分钟`;
  const hours = Math.floor(minutes / 60);
  const remainingMinutes = minutes % 60;
  return remainingMinutes > 0 ? `${hours}小时${remainingMinutes}分` : `${hours}小时`;
}

function isTextPart(part: MessagePart): part is TextMessagePart {
  return part.kind === "text";
}

function toolDiffPaths(tool: ToolCallView): string[] {
  return (tool.content ?? []).flatMap((content) =>
    content.type === "diff" && content.diff.path ? [content.diff.path] : [],
  );
}

export function activeToolLabel(tool: ToolCallView): string {
  if (isToolDiscovery(tool)) return "查找可用工具";
  switch (detectToolCallRenderer(tool)) {
    case "command":
      return "正在运行命令";
    case "edit":
      return "正在修改文件";
    case "read":
      return "正在读取项目文件";
    case "search":
      return "正在检索资料";
    case "weather":
      return "正在查询天气";
    case "task":
      return "子代理正在处理";
    case "defer-execute":
      return "正在准备批量操作";
    case "send-message":
    case "agent-mail":
      return "正在发送消息";
    case "image-gen":
      return "正在生成图像";
    case "visualizer":
      return "正在生成可视化";
    case "team-create":
    case "team-delete":
    case "team-status":
      return "正在协调团队";
    case "specialist":
      return "正在查找专家";
    case "knowledge":
      return "正在检索组织知识";
    case "personal-knowledge":
      return "正在检索个人知识";
    default:
      return "正在执行操作";
  }
}
