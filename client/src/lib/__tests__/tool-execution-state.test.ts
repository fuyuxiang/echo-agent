import { describe, expect, it } from "vitest";
import type { ToolCallView } from "@/stores/session-store";
import { executionToolStates } from "../tool-execution-state";
import { summarizeExecutionProcess } from "../execution-process";
import { getExecutionNotice } from "../execution-notice";
import type { AutomationStatus } from "../automation-client";

const dispatch = (receiptId = "action-1"): ToolCallView => ({
  toolCallId: "dispatch", kind: "other", toolName: "computer_click", title: "点击按钮", status: "completed",
  content: [{ type: "text", text: JSON.stringify({ status: "dispatched_unverified", receiptId }) }],
});

describe("tool lifetimes follow their execution owner", () => {
  const native: ToolCallView = { toolCallId: "native", kind: "other", toolName: "computer_screenshot", title: "读取应用界面", status: "in_progress", content: [] };
  const read: ToolCallView = { toolCallId: "read", kind: "read_file", title: "Read /work/source.md", status: "in_progress", content: [] };
  const status = (phase: NonNullable<AutomationStatus["task"]>["phase"], authorized = true): AutomationStatus => ({
    sessionId: "session", mode: "computer_use", paused: false, allowPrivateNetwork: false,
    browser: { available: true }, computer: { available: true, platform: "macOS", screenCapture: true, inputControl: true },
    browserRunning: false, browserHasData: false, task: { phase, authorized, scope: "核对来源", reason: null, generation: 1 },
  });

  it.each(["paused", "stopped", "failed", "completed", "result_unknown", "waiting_user"] as const)("%s only retires native operations when the model is still working", phase => {
    const states = executionToolStates([native, read], false, status(phase, false));
    expect(states.get("native")?.state).toBe("pending");
    expect(states.get("read")?.state).toBe("in_progress");
  });

  it("a native task can remain live after the model message ends without reviving other tools", () => {
    const states = executionToolStates([native, read], true, status("running"));
    expect(states.get("native")?.state).toBe("in_progress");
    expect(states.get("read")?.state).toBe("pending");
  });

  it.each([
    { kind: "browser_snapshot", title: "读取网页" },
    { kind: "other", toolName: "echoagent-automation__browser_snapshot", title: "读取网页" },
    { kind: "other", toolName: "use_tool", rawInput: { variant: "UseTool", tool_name: "echoagent-automation__computer_screenshot" }, title: "读取应用" },
    { kind: "other", title: "computer_screenshot" },
  ])("recognizes native tool identities across transport formats: %j", identity => {
    const call = { ...native, toolName: undefined, ...identity };
    expect(executionToolStates([call], true, status("running")).get("native")?.state).toBe("in_progress");
  });

  it("tool titles and command arguments cannot transfer ownership to a native task", () => {
    const command = { ...read, kind: "bash", toolName: "bash", title: "computer_screenshot", rawInput: { command: "computer_screenshot" } };
    expect(executionToolStates([command], true, status("running")).get("read")?.state).toBe("pending");
    expect(executionToolStates([{ ...command, rawInput: { tool_name: "computer_screenshot" } }], true, status("running")).get("read")?.state).toBe("pending");
    expect(executionToolStates([{ ...native, status: "completed" }], false, status("stopped", false)).get("native")?.state).toBe("completed");
  });
});
const completion = (receiptId = "action-1", toolName = "automation_complete"): ToolCallView => ({
  toolCallId: "complete", kind: "other", toolName, title: "核验成果", status: "completed",
  content: [{ type: "text", text: JSON.stringify({ task: { phase: "completed", receipts: [{ id: receiptId }] } }) }],
});

describe("host verification resolves only its own earlier receipts", () => {
  it("宿主完成凭证解除先前待核验提示，原始派发记录保持不变", () => {
    const tools = [dispatch(), completion()];
    const snapshot = structuredClone(tools);
    const states = executionToolStates(tools);
    expect(states.get("dispatch")).toMatchObject({ state: "completed", label: "后续任务已核验" });
    const parts = tools.map(toolCall => ({ kind: "tool_call" as const, toolCall }));
    expect(summarizeExecutionProcess(parts, false, "end_turn")).toMatchObject({ state: "complete", pendingToolCount: 0 });
    expect(getExecutionNotice({ id: "turn", role: "assistant", parts, complete: true, stopReason: "end_turn" })).toBeNull();
    expect(tools).toEqual(snapshot);
  });

  it.each(["computer_screenshot", "automation_status", "web_search", "browser_automation_complete"])("%s 的同名内容不能替代宿主任务核验", name => {
    expect(executionToolStates([dispatch(), completion("action-1", name)]).get("dispatch")?.state).toBe("pending");
  });

  it("完成回执自身结果不确定时不能解除其他操作的不确定性", () => {
    const pending = { ...completion(), content: [{ type: "text" as const, text: '{"status":"result_unknown","task":{"phase":"completed","receipts":[{"id":"action-1"}]}}' }] };
    expect(executionToolStates([dispatch(), pending]).get("dispatch")?.state).toBe("pending");
  });

  it("其他任务、较早的核验与失败的完成请求不能解除待核验状态", () => {
    for (const tools of [[dispatch(), completion("other-task")], [completion(), dispatch()],
      [dispatch(), { ...completion(), status: "failed" as const }]]) {
      expect(executionToolStates(tools).get("dispatch")?.state).toBe("pending");
    }
  });

  it("缺少关联凭证时保留结果未知，不从模型成功声明推断核验", () => {
    const unknown: ToolCallView = { ...dispatch(), status: "failed", content: [{ type: "text", text: '{"status":"result_unknown"}' }] };
    expect(executionToolStates([unknown, completion()]).get("dispatch")?.state).toBe("pending");
    const receipted = { ...unknown, content: [{ type: "text" as const, text: '{"status":"result_unknown","receiptId":"action-1"}' }] };
    expect(executionToolStates([receipted, completion()]).get("dispatch"))
      .toMatchObject({ state: "completed", label: "后续任务已核验" });
  });
});
