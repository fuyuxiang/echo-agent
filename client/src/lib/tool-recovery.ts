import type { MessagePart, ToolCallView } from "@/stores/session-store";
import { executionToolStates } from "./tool-execution-state";

export interface ToolRecovery {
  failedToolCallId: string;
  successfulToolCallId: string;
}

/** Compare complete inputs, never just a tool category, filename or reply text. */
function buildOperationKey(tool: ToolCallView): string | null {
  if (!tool.toolCallId || tool.rawInput == null) return null;
  let input = tool.rawInput;
  if (typeof input === "string") {
    try { input = JSON.parse(input); } catch { return null; }
  }
  if (!input || typeof input !== "object") return null;
  if ("truncated" in input && input.truncated === true) return null;
  // Canonical names come from Runtime metadata. Legacy records need both a
  // specific display identity and actual parameters; generic "other" is unsafe.
  const identity = tool.toolName ? ["name",tool.toolName]
    : tool.kind && !["other","unknown"].includes(tool.kind) && tool.title && Object.keys(input).length
      ? ["legacy",tool.kind,tool.title] : null;
  if (!identity) return null;
  let nodes = 0;
  const normalize = (value: unknown, depth = 0): unknown => {
    if (++nodes > 10_000 || depth > 32) throw new Error("input too complex");
    if (value === null || typeof value === "string" || typeof value === "boolean") return value;
    if (typeof value === "number" && Number.isFinite(value)) return value;
    if (Array.isArray(value)) return ["array",value.map(item => normalize(item,depth+1))];
    if (value && typeof value === "object") {
      return ["object",Object.keys(value).sort().map(key => [key,normalize((value as Record<string,unknown>)[key],depth+1)])];
    }
    throw new Error("non-JSON input");
  };
  try {
    const key = JSON.stringify([identity,normalize(input)]);
    return key.length <= 256_000 ? key : null;
  } catch { return null; }
}

// Tool objects are immutable between updates; streamed answer tokens should
// not repeatedly serialize every historical tool's potentially large input.
const operationKeys = new WeakMap<ToolCallView,string | null>();
function operationKey(tool: ToolCallView): string | null {
  if (operationKeys.has(tool)) return operationKeys.get(tool) ?? null;
  const key = buildOperationKey(tool);
  operationKeys.set(tool,key);
  return key;
}

/** A later start proves retry order; parallel successes cannot clear failures. */
function beganAfterFailure(failure: Extract<MessagePart,{kind:"tool_call"}>, success: Extract<MessagePart,{kind:"tool_call"}>): boolean {
  const finished = failure.toolCall.finishedAt;
  const started = success.toolCall.startedAt;
  if (finished != null && started != null) return started > finished;
  // Durable records can omit timestamps. Distinct inference generations also
  // prove sequential attempts; tools from the same generation may be parallel.
  return Boolean(failure.streamId && success.streamId && failure.streamId !== success.streamId);
}

export function findToolRecoveries(parts: MessagePart[]): ToolRecovery[] {
  const tools = parts.filter((part): part is Extract<MessagePart,{kind:"tool_call"}> => part.kind === "tool_call");
  const execution = executionToolStates(tools.map(part => part.toolCall));
  const states = tools.map(part => execution.get(part.toolCall.toolCallId)!.state);
  if (!states.includes("failed")) return [];
  const keys = tools.map((part, index) => states[index] === "in_progress" || states[index] === "pending" ? null : operationKey(part.toolCall));
  const successful = new Map<string,number[]>();
  keys.forEach((key,index) => {
    if (key && states[index] === "completed") {
      const candidates = successful.get(key) ?? [];
      candidates.push(index);
      successful.set(key,candidates);
    }
  });
  return tools.flatMap((part,index) => {
    if (states[index] !== "failed" || !keys[index]) return [];
    const replacement = successful.get(keys[index]!)?.find(candidate => candidate > index && beganAfterFailure(part,tools[candidate]));
    return replacement == null ? [] : [{failedToolCallId:part.toolCall.toolCallId,successfulToolCallId:tools[replacement].toolCall.toolCallId}];
  });
}
