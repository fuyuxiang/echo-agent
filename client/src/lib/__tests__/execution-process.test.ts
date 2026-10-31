import { describe, expect, it } from "vitest";
import {
  formatProcessDuration,
  partitionAssistantParts,
  summarizeExecutionProcess,
} from "../execution-process";
import type { MessagePart } from "@/stores/session-store";

const tool = (
  status: "in_progress" | "completed" | "failed",
  kind = "edit_file",
): MessagePart => ({
  kind: "tool_call",
  toolCall: {
    toolCallId: `${kind}-${status}`,
    title: "Edit src/App.tsx",
    kind,
    status,
    content: [{
      type: "diff",
      diff: { path: "src/App.tsx", old: "a", new: "b" },
    }],
  },
});

describe("partitionAssistantParts", () => {
  it("紧凑展示把各轮公开进展与对应工具归组，保留原始公开文本供导出", () => {
    const parts: MessagePart[] = [
      { kind: "text", text: "先核对票价。", streamId: "1" },
      { ...tool("completed"), streamId: "1" },
      { kind: "text", text: "补齐开放时间。", streamId: "2" },
      { ...tool("completed"), streamId: "2" },
      { kind: "text", text: "完整攻略如下。", streamId: "3" },
    ];
    const display = partitionAssistantParts(parts, { organizeProgress: true });
    expect(display.processParts).toEqual(parts.slice(0, 4));
    expect(display.responseParts.map(part => part.text)).toEqual(["完整攻略如下。"]);
    expect(display.answerStarted).toBe(true);
    expect(partitionAssistantParts(parts).responseParts).toHaveLength(3);
    expect(parts[0]).not.toHaveProperty("phase");
  });

  it("紧凑展示不会吞掉显式阶段成果或同轮最终答复", () => {
    const parts: MessagePart[] = [
      { kind: "text", phase: "partial_answer", text: "已核实的票价", streamId: "1" },
      { ...tool("in_progress"), streamId: "1" },
      { kind: "text", phase: "final_answer", text: "请确认日期", streamId: "1" },
    ];
    expect(partitionAssistantParts(parts, { organizeProgress: true }).responseParts).toEqual([parts[0], parts[2]]);
  });

  it("同轮工具后的说明及没有生成标识的历史进展均可在完整过程中阅读", () => {
    const parts: MessagePart[] = [
      { kind: "text", text: "原始进展" }, tool("completed"),
      { kind: "text", text: "这一轮的说明", streamId: "1" },
      { ...tool("failed"), streamId: "1" },
      { kind: "text", text: "同轮后续说明", streamId: "1" },
    ];
    const display = partitionAssistantParts(parts, { organizeProgress: true });
    expect(display.processParts).toEqual(parts);
    expect(display.responseParts).toEqual([]);
    expect(display.answerStarted).toBe(false);
  });

  it("同轮工具之后的未分类文本保留在正文，但不误报最终答复", () => {
    const result = partitionAssistantParts([
      { ...tool("completed"), streamId: "1" },
      { kind: "text", text: "先确认可用工具，同时核对出行条件。", streamId: "1" },
    ]);
    expect(result.processParts).toHaveLength(1);
    expect(result.responseParts.map(part => part.text)).toEqual(["先确认可用工具，同时核对出行条件。"]);
    expect(result.answerStarted).toBe(false);
  });
  it("显式阶段成果和最终答复优先于同轮工具推断，继续执行也不隐藏成果", () => {
    const result = partitionAssistantParts([
      { kind: "text", phase: "commentary", text: "准备查找资料", streamId: "1" },
      { kind: "text", phase: "partial_answer", text: "已确认的交通方案", streamId: "1" },
      { ...tool("in_progress"), streamId: "1" },
      { kind: "text", phase: "final_answer", text: "请确认你的出行日期？", streamId: "1" },
    ]);
    expect(result.responseParts.map(part => part.text)).toEqual(["已确认的交通方案", "请确认你的出行日期？"]);
    expect(result.processParts.map(part => part.kind)).toEqual(["text", "tool_call"]);
    expect(result.answerStarted).toBe(true);
  });
  it("阶段成果不触发最终答复的自动折叠，未知生成保守保留", () => {
    const result = partitionAssistantParts([
      { kind: "text", text: "交通已核实", streamId: "1" },
      { ...tool("completed"), streamId: "2" },
      { kind: "text", phase: "partial_answer", text: "景点开放信息", streamId: "2" },
    ]);
    expect(result.responseParts.map(part => part.text)).toEqual(["交通已核实", "景点开放信息"]);
    expect(result.answerStarted).toBe(false);
  });
  it("旧事件缺少语义标记时保留工具前后的公开文本", () => {
    const result = partitionAssistantParts([
      { kind: "text", text: "我先检查项目。" },
      { kind: "thought", text: "需要确认入口。" },
      tool("completed"),
      { kind: "text", text: "修复已经完成。" },
    ]);

    expect(result.processParts.map(part => part.kind)).toEqual(["thought", "tool_call"]);
    expect(result.responseParts.map((part) => part.text)).toEqual(["我先检查项目。", "修复已经完成。"]);
  });

  it("生成标识只用于判断答复状态，不代替公开文本的语义分类", () => {
    const firstTool = tool("completed");
    const result = partitionAssistantParts([
      { kind: "text", text: "I should inspect the project first.", streamId: "generation-1" },
      { ...firstTool, streamId: "generation-1" },
      { kind: "thought", text: "整理最终结论", streamId: "generation-2" },
      { kind: "text", text: "这是最终答案。", streamId: "generation-2" },
    ]);

    expect(result.processParts.map((part) => part.kind)).toEqual([
      "tool_call",
      "thought",
    ]);
    expect(result.responseParts.map((part) => part.text)).toEqual(["I should inspect the project first.", "这是最终答案。"]);
    expect(result.answerStarted).toBe(true);
  });

  it.each([undefined, "same-generation"])("阶段成果先于后续工具出现时仍保留：%s", streamId => {
    const result = partitionAssistantParts([
      { kind: "text", text: "已核实的交通方案", streamId },
      { ...tool("in_progress"), streamId },
    ]);
    expect(result.responseParts.map(part => part.text)).toEqual(["已核实的交通方案"]);
    expect(result.answerStarted).toBe(false);
  });

  it("只有明确的进展说明进入过程，空白尾块不改变答复状态", () => {
    const result = partitionAssistantParts([
      { kind: "text", text: "已确认的成果", streamId: "1" },
      { kind: "text", phase: "commentary", text: "继续检查", streamId: "2" },
      { kind: "text", text: "  ", streamId: "2" },
    ]);
    expect(result.processParts.map(part => part.kind === "text" ? part.text : "")).toEqual(["继续检查"]);
    expect(result.responseParts[0].text).toBe("已确认的成果");
    expect(result.answerStarted).toBe(false);
  });

  it("拆分同一文本块中的 think 标签和正式答案", () => {
    const result = partitionAssistantParts([{
      kind: "text",
      text: "<think>Internal English analysis.\nCheck the timeout.</think>\n\n调用大模型超时通常由网络问题导致。",
      streamId: "generation-1",
    }]);

    expect(result.processParts).toEqual([{
      kind: "thought",
      text: "Internal English analysis.\nCheck the timeout.",
      streamId: "generation-1",
    }]);
    expect(result.responseParts).toEqual([{
      kind: "text",
      text: "调用大模型超时通常由网络问题导致。",
      streamId: "generation-1",
    }]);
  });

  it("流式 think 标签未闭合时也只当作思考过程", () => {
    const result = partitionAssistantParts([{
      kind: "text",
      text: "<think>Still reasoning",
      streamId: "generation-1",
    }]);

    expect(result.processParts).toEqual([{
      kind: "thought",
      text: "Still reasoning",
      streamId: "generation-1",
    }]);
    expect(result.responseParts).toEqual([]);
  });

  it.each([
    ["thinking", "<thinking>Reasoning</thinking>Answer"],
    ["reasoning", "<REASONING mode=\"deep\">Reasoning</REASONING>Answer"],
    ["analysis", "<analysis>Reasoning</analysis>Answer"],
  ])("兼容 %s 推理标签", (_tag, text) => {
    const result = partitionAssistantParts([{ kind: "text", text }]);
    expect(result.processParts).toEqual([{ kind: "thought", text: "Reasoning" }]);
    expect(result.responseParts).toEqual([{ kind: "text", text: "Answer" }]);
  });

  it("不会误伤正式答案中间的标签示例", () => {
    const text = "XML 示例：<think>demo</think>";
    const result = partitionAssistantParts([{ kind: "text", text }]);
    expect(result.processParts).toEqual([]);
    expect(result.responseParts).toEqual([{ kind: "text", text }]);
  });

  it("没有尾部答复时不把已有文本藏进折叠过程", () => {
    const result = partitionAssistantParts([
      { kind: "text", text: "这是可见结果" },
      tool("completed"),
    ]);

    expect(result.responseParts.map((part) => part.text)).toEqual(["这是可见结果"]);
    expect(result.processParts.every((part) => part.kind !== "text")).toBe(true);
  });

  it("普通问答不生成过程组", () => {
    const result = partitionAssistantParts([{ kind: "text", text: "直接回答" }]);
    expect(result.processParts).toEqual([]);
    expect(result.responseParts[0].text).toBe("直接回答");
  });
});

describe("summarizeExecutionProcess", () => {
  it("用语义状态描述当前操作并统计文件", () => {
    const summary = summarizeExecutionProcess([
      { kind: "thought", text: "分析" },
      tool("completed"),
      tool("in_progress"),
    ], true);

    expect(summary.title).toBe("正在修改文件");
    expect(summary.state).toBe("running");
    expect(summary.completedToolCount).toBe(1);
    expect(summary.toolCount).toBe(2);
    expect(summary.changedFiles).toEqual(["src/App.tsx"]);
  });

  it("所有操作均未成功时明确展示失败", () => {
    const summary = summarizeExecutionProcess([tool("failed", "bash")], false);
    expect(summary.state).toBe("failed");
    expect(summary.title).toBe("操作执行未完成");
  });

  it("用户取消后明确显示已停止，而不是伪装成完成", () => {
    const summary = summarizeExecutionProcess([tool("completed", "bash")], false, "cancelled");
    expect(summary.state).toBe("stopped");
    expect(summary.title).toBe("已停止执行");
  });

  it("区分权限拒绝与安全规则阻止", () => {
    const rejected = summarizeExecutionProcess([], false, "cancelled", "PermissionRejected");
    const blocked = summarizeExecutionProcess([], false, "cancelled", "HookDenied");
    expect(rejected.state).toBe("stopped");
    expect(rejected.title)
      .toBe("权限未批准，执行已停止");
    expect(blocked).toMatchObject({
      state: "failed",
      title: "执行被安全规则阻止",
    });
  });

  it("终止时仍有未收尾操作会标记为需要注意", () => {
    const summary = summarizeExecutionProcess([tool("in_progress", "bash")], false, "end_turn");
    expect(summary.state).toBe("attention");
    expect(summary.description).toContain("未记录完成结果");
  });

  it("成功与未恢复失败混合时为部分完成，正文声明成功不改变判定", () => {
    const summary = summarizeExecutionProcess([tool("completed"),tool("failed","bash"),{kind:"text",text:"全部完成！"}],false,"end_turn");
    expect(summary.state).toBe("partial");
    expect(summary.title).toBe("部分操作未完成");
    expect(summary.unresolvedToolCount).toBe(1);
  });
  it.each(["error","rate_limit","max_tokens","max_turns","refusal"])("即使已有成功工具，%s 终止也不能判定完成", reason => {
    expect(summarizeExecutionProcess([tool("completed")],false,reason).state).toBe("failed");
  });
  it("失败后仍在继续运行时显示当前进度，不提前宣告失败", () => {
    const summary = summarizeExecutionProcess([tool("failed"),tool("in_progress","bash")],true);
    expect(summary.state).toBe("running");
    expect(summary.title).toBe("正在运行命令");
  });
  it("缺失或未知结束状态不伪装为完成", () => {
    expect(summarizeExecutionProcess([tool("completed")],false,undefined,undefined,undefined,{settled:false}).state).toBe("attention");
    expect(summarizeExecutionProcess([tool("completed")],false,"unknown-future-reason").state).toBe("attention");
    expect(summarizeExecutionProcess([],false,"cancelled","session_replay_incomplete").title).toBe("执行记录不完整");
  });
  it("恢复全部中间失败后展示普通执行过程，终止错误仍优先", () => {
    const parts: MessagePart[] = [
      {kind:"tool_call",toolCall:{toolCallId:"failed",kind:"read",title:"Read report.md",status:"failed",content:[],rawInput:{path:"report.md"},finishedAt:1_000}},
      {kind:"tool_call",toolCall:{toolCallId:"retry",kind:"read",title:"Read report.md",status:"completed",content:[],rawInput:{path:"report.md"},startedAt:2_000}},
    ];
    expect(summarizeExecutionProcess(parts,false,"end_turn")).toMatchObject({state:"complete",title:"执行过程",unresolvedToolCount:0,failedToolCount:1});
    expect(summarizeExecutionProcess(parts,false,"error").state).toBe("failed");
  });
});

describe("formatProcessDuration", () => {
  it("生成人类可读时长", () => {
    expect(formatProcessDuration(8_900)).toBe("8秒");
    expect(formatProcessDuration(65_000)).toBe("1分5秒");
  });
});
