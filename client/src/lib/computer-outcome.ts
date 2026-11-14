import type { ToolCallView } from "@/stores/session-store";
export interface ComputerOutcome {
  label: string;
  verified: boolean;
  unknown: boolean;
  pendingVerification: boolean;
  receiptId?: string;
  status: string;
  verifiedTaskReceiptIds?: string[];
}

/** Transport completion is independent of the physical action's outcome. */
export function computerOutcome(tool: ToolCallView): ComputerOutcome | null {
  const input = tool.rawInput as { tool_name?: unknown } | undefined;
  const name = (typeof input?.tool_name === "string" ? input.tool_name : undefined) ?? tool.toolName ?? tool.title;
  if (!/^(?:echoagent-automation__)?(?:computer_|browser_|automation_complete)/.test(name)) return null;
  const action = /(?:computer_(?:click|click_control|move|scroll|drag|type|key|activate_window|key_window|file_dialog|open_app|invoke|set_value)|browser_(?:click|type|key|upload|select|drag|navigate|scroll|hover|tabs))$/.test(name);
  for (const block of tool.content) {
    if (block.type !== "text") continue;
    const unknownTransport = tool.status === "failed" && action && /native_timeout|native_failure|helper_(?:disconnected|invalid_response)|browser_extension_(?:timeout|disconnected)|cancelled|canceled|result_unknown|outcome unknown|result unknown|结果未知|浏览器.*超时/i.test(block.text);
    const unknownOutcome: ComputerOutcome = { label: "结果待核验", verified: false, unknown: true, pendingVerification: true, status: "result_unknown" };
    let value: Record<string, unknown>;
    try { value = JSON.parse(block.text); } catch { if (unknownTransport) return unknownOutcome; continue; }
    if (!value || typeof value !== "object" || Array.isArray(value)) {
      if (unknownTransport) return unknownOutcome;
      continue;
    }
    const task = value.task && typeof value.task === "object" && !Array.isArray(value.task)
      ? value.task as Record<string, unknown> : undefined;
    const completed = /^(?:echoagent-automation__)?automation_complete$/.test(name) && task?.phase === "completed";
    const status = typeof value.status === "string" ? value.status : completed ? "completed" : "observed";
    const unknown = unknownTransport || status === "result_unknown" || status === "timeout";
    const verified = value.verified === true || completed;
    const dispatched = status === "executed_unverified" || status === "dispatched_unverified" ||
      value.dispatched === true || value.executed === true || value.clicked === true ||
      value.typed === true || !!value.pressed;
    const label = unknown ? "结果待核验" : completed ? "成果已核验" : verified ? "已核验" :
      status === "failed" ? "未完成" : status === "prepared_not_submitted" ? "已准备，等待提交" :
        dispatched ? "已派发，待核验" : "已观察";
    return {
      label, verified, unknown, pendingVerification: unknown || (!verified && dispatched), status,
      receiptId: typeof value.receiptId === "string" ? value.receiptId : undefined,
      ...(completed ? { verifiedTaskReceiptIds: Array.isArray(task?.receipts)
        ? task.receipts.flatMap(receipt => receipt && typeof receipt === "object"
          && typeof receipt.id === "string" ? [receipt.id] : []) : [] } : {}),
    };
  }
  return null;
}
