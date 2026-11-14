import { computerOutcome } from "./computer-outcome";
import { detectToolCallRenderer, isToolDiscovery, summarizeTool, type ToolRenderer } from "./tool-renderers";
import type { MessagePart, ToolCallView } from "@/stores/session-store";

export interface ExecutionActivity {
  id: string;
  tools: ToolCallView[];
  renderer: ToolRenderer;
  label: string;
}

function inputFor(tool: ToolCallView): Record<string, unknown> | undefined {
  const raw = tool.rawInput;
  if (!raw || typeof raw !== "object" || Array.isArray(raw)) return undefined;
  const input = raw as Record<string, unknown>;
  return input.tool_input && typeof input.tool_input === "object" && !Array.isArray(input.tool_input)
    ? input.tool_input as Record<string, unknown> : input;
}

function operationKey(tool: ToolCallView): string {
  const raw = tool.rawInput as Record<string, unknown> | undefined;
  return (typeof raw?.tool_name === "string" ? raw.tool_name : tool.toolName ?? tool.kind).toLowerCase();
}

/** Keep useful file/command context without filling the live view with paths. */
export function executionActivityLabel(tool: ToolCallView): string {
  const renderer = detectToolCallRenderer(tool);
  if (isToolDiscovery(tool)) return "查找可用工具";
  const input = inputFor(tool);
  if (renderer === "read" && /^(read_file|read)$/.test(operationKey(tool)) || renderer === "edit") {
    const path = input?.path ?? input?.file_path;
    const title = typeof path === "string" ? path : tool.title.replace(/^(?:Read|Write|Edit)\s+/i, "");
    // A search expression is not a filename. Only shorten explicit paths.
    if (typeof path === "string" || /^(?:Read|Write|Edit)\s+/i.test(tool.title)) {
      const segments = title.replace(/^[`"']|[`"']$/g, "").split(/[\\/]/).filter(Boolean);
      return segments[segments.length - 1] ?? title;
    }
  }
  if (renderer === "command") {
    const command = input?.command ?? input?.cmd;
    return typeof command === "string" ? command : tool.title.replace(/^(?:Run|Execute)\s+/i, "");
  }
  return summarizeTool(tool, renderer) || "执行操作";
}

const groupLabels: Partial<Record<ToolRenderer, string>> = { read: "文件读取", search: "资料检索" };

/** Only adjacent, confirmed operations of the same kind may be summarized.
 * The full record is untouched, including repeats, retries and commentary. */
export function executionActivities(parts: MessagePart[], heldActivities: readonly ExecutionActivity[] = []): ExecutionActivity[] {
  const activities: ExecutionActivity[] = [];
  // Preserve the identity and membership of rows being read. Their receipts
  // still update, while new calls remain free to form their own groups.
  const heldByTool = new Map(heldActivities.flatMap(activity => activity.tools.map(tool => [tool.toolCallId, activity] as const)));
  const retained = new Map<string, ExecutionActivity>();
  let previousKey: string | undefined;
  for (const part of parts) {
    if (part.kind !== "tool_call") {
      if (part.text.trim()) previousKey = undefined;
      continue;
    }
    const tool = part.toolCall;
    const held = heldByTool.get(tool.toolCallId);
    if (held) {
      let activity = retained.get(held.id);
      if (!activity) {
        activity = { ...held, tools: [] };
        retained.set(held.id, activity);
        activities.push(activity);
      }
      activity.tools.push(tool);
      previousKey = undefined;
      continue;
    }
    const renderer = detectToolCallRenderer(tool);
    const outcome = computerOutcome(tool);
    const operation = operationKey(tool);
    const groupable = renderer === "read" ? /^(read_file|read)$/.test(operation) : Boolean(groupLabels[renderer]);
    const key = tool.status === "completed" && !tool.documentProgress && !outcome
      && groupable && !isToolDiscovery(tool) ? operation : undefined;
    const previous = activities[activities.length - 1];
    if (key && key === previousKey && previous?.renderer === renderer) {
      previous.tools.push(tool);
      // Count actual calls, rather than claiming distinct files or web results.
      previous.label = `${groupLabels[renderer]} · ${previous.tools.length} 次操作`;
    } else {
      activities.push({ id: tool.toolCallId, tools: [tool], renderer, label: executionActivityLabel(tool) });
    }
    previousKey = key;
  }
  return activities;
}
