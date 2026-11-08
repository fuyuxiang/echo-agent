import type { ChatMessage } from "@/stores/session-store";
import type { SubagentRuntime } from "@/stores/subagent-store";

export type TurnOrigin = "user" | "background_task" | "subagent" | "notification" | "goal" | "scheduled" | "plan_resume" | "runtime";
/** Native provenance only. Runtime and each business workflow own lifecycle. */
export interface ExecutionTurn { origin: TurnOrigin; sourceId?: string; }
export interface TurnIdentity extends ExecutionTurn { promptIndex?: number; }
const origins = new Set<TurnOrigin>(["user", "background_task", "subagent", "notification", "goal", "scheduled", "plan_resume", "runtime"]);
export const validPromptIndex = (value: unknown): value is number => typeof value === "number" && Number.isSafeInteger(value) && value >= 0;

/** Compatibility with persisted Runtime identities, never inferred from prose. */
export function inferTurnOrigin(promptId?: string): ExecutionTurn {
  if (promptId?.startsWith("task-completed-")) return { origin: "background_task", sourceId: promptId.slice(15) };
  if (promptId?.startsWith("subagent-completed-")) return { origin: "subagent", sourceId: promptId.slice(19) };
  if (promptId?.startsWith("notifications-")) return { origin: "notification" };
  if (promptId?.startsWith("goal-summary-") || promptId?.startsWith("goal-classifier-nudge-")) return { origin: "goal" };
  if (promptId?.startsWith("workflow-completed-")) return { origin: "runtime" };
  if (promptId?.startsWith("scheduler-fired-")) return { origin: "scheduled" };
  if (promptId?.startsWith("plan-resume-")) return { origin: "plan_resume" };
  return { origin: "user" };
}

export function readTurnIdentity(meta: Record<string, unknown> | undefined, promptId?: string): TurnIdentity {
  const raw = meta?.turnOrigin;
  const context = raw && typeof raw === "object" ? raw as Record<string, unknown> : {};
  const inferred = inferTurnOrigin(promptId);
  const origin = origins.has(context.origin as TurnOrigin) ? context.origin as TurnOrigin : inferred.origin;
  const index = meta?.promptIndex ?? meta?.prompt_index;
  return { origin, sourceId: typeof context.sourceId === "string" && context.sourceId.trim() ? context.sourceId.trim().slice(0, 4096) : inferred.sourceId,
    ...(validPromptIndex(index) ? { promptIndex: index } : {}) };
}

export function resolveExecutionTurn(message: ChatMessage): ExecutionTurn {
  return message.execution ?? inferTurnOrigin(message.promptId);
}

/** Read source links from existing tool/subagent records; do not duplicate their
 * states or guess that a new user prompt has consumed a pending result. */
export function executionSource(message: ChatMessage, messages: ChatMessage[], subagents: SubagentRuntime[] = []): { description?: string; rootPromptId?: string } {
  const seen = new Set<string>();
  let current: ChatMessage | undefined = message;
  let description: string | undefined;
  while (current?.promptId && !seen.has(current.promptId)) {
    seen.add(current.promptId);
    const turn = resolveExecutionTurn(current);
    if (turn.origin === "user") return { description, rootPromptId: current.promptId };
    let parent: string | undefined;
    if (turn.sourceId && turn.origin === "background_task") {
      const owner = messages.find(m => m.parts.some(p => p.kind === "tool_call" && p.toolCall.backgroundTask?.id === turn.sourceId));
      const tool = owner?.parts.find(p => p.kind === "tool_call" && p.toolCall.backgroundTask?.id === turn.sourceId);
      description ??= tool?.kind === "tool_call" ? tool.toolCall.backgroundTask?.description || tool.toolCall.title : undefined;
      parent = owner?.promptId;
    } else if (turn.sourceId && turn.origin === "subagent") {
      const agent = subagents.find(a => a.id === turn.sourceId);
      description ??= agent?.description || undefined;
      parent = agent?.parentPromptId;
    }
    current = parent ? messages.find(m => m.promptId === parent && m.role === "assistant")
      ?? messages.find(m => m.promptId === parent && m.role === "user") : undefined;
  }
  return { description };
}

export function turnPresentation(turn: ExecutionTurn, description?: string): { label: string; description?: string } | undefined {
  const labels: Record<TurnOrigin, string> = { user: "", background_task: "后台结果核对", subagent: "子任务结果核对", notification: "后台通知处理",
    goal: "继续推进任务", scheduled: "定时任务执行", plan_resume: "按确认的计划继续", runtime: "后续处理" };
  return turn.origin === "user" ? undefined : { label: labels[turn.origin], description };
}
