import type { ToolCallView } from "@/stores/session-store";
import { computerOutcome, type ComputerOutcome } from "./computer-outcome";

/** A completed transport does not prove that a desktop action succeeded. */
function stateFromOutcome(tool: ToolCallView, outcome: ComputerOutcome | null, executionEnded: boolean, verifiedLater = false) {
  const pending = outcome?.unknown || tool.status === "completed" && outcome?.pendingVerification
    || executionEnded && tool.status === "in_progress";
  const state = verifiedLater && outcome?.pendingVerification ? "completed"
    : pending ? "pending" : outcome?.status === "failed" ? "failed" : tool.status;
  const label = verifiedLater && outcome?.pendingVerification ? "后续任务已核验"
    : state === "pending" ? (outcome?.pendingVerification ? outcome.label : "结果待确认")
    : state === "completed" ? outcome?.label ?? "完成"
      : state === "failed" ? "失败" : "运行中";
  return { state, label, outcome };
}

export function toolExecutionState(tool: ToolCallView, executionEnded = false) {
  return stateFromOutcome(tool, computerOutcome(tool), executionEnded);
}

export type ToolExecutionState = ReturnType<typeof toolExecutionState>;

/** Only a later host completion naming this exact receipt can resolve an
 * earlier dispatch. Other tasks, screenshots and model prose are insufficient. */
export function executionToolStates(tools: readonly ToolCallView[], executionEnded = false): ReadonlyMap<string, ToolExecutionState> {
  const states = new Map<string, ToolExecutionState>();
  const verifiedReceipts = new Set<string>();
  for (let index = tools.length - 1; index >= 0; index--) {
    const tool = tools[index];
    const outcome = computerOutcome(tool);
    const state = stateFromOutcome(tool, outcome, executionEnded, Boolean(outcome?.receiptId && verifiedReceipts.has(outcome.receiptId)));
    states.set(tool.toolCallId, state);
    if (tool.status === "completed" && state.state === "completed") outcome?.verifiedTaskReceiptIds?.forEach(id => verifiedReceipts.add(id));
  }
  return states;
}
