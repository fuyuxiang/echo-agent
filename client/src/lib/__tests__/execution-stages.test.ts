import { describe, expect, it } from "vitest";
import { executionStages, executionStageState, stageOperationSummary } from "../execution-stages";
import type { MessagePart, ToolCallView } from "@/stores/session-store";

const discovery = (id: string, status: ToolCallView["status"] = "completed"): MessagePart => ({
  kind: "tool_call", streamId: "1", toolCall: { toolCallId: id, toolName: "SearchTool", kind: "other",
    title: 'Search tools: "南京 天气"', status, content: [] },
});
describe("execution stages", () => {
  it("同一生成中部分事件缺失计划元数据时，不把说明和工具拆散", () => {
    const stages = executionStages([
      { kind: "text", text: "核对来源和关键条件", streamId: "1", stageTitle: "确认条件" },
      { kind: "thought", text: "并行执行", streamId: "1" },
      { ...discovery("a"), stageTitle: "确认条件" },
    ]);
    expect(stages).toHaveLength(1);
    expect(stages[0].progress[0].text).toBe("核对来源和关键条件");
    expect(stages[0].title).toBe("确认条件");
    expect(stages[0].planned).toBe(true);
  });
  it("说明与工具按实际生成归组，原始思考保持为次要信息", () => {
    const stages = executionStages([
      { kind: "thought", text: "让我并行发起", streamId: "1" }, discovery("a"),
      { kind: "text", text: "核对实时信息来源", streamId: "1" }, discovery("b"),
      { kind: "text", text: "正在整理行程", streamId: "2" },
    ]);
    expect(stages).toHaveLength(2);
    expect(stages[0].progress[0].text).toBe("核对实时信息来源");
    expect(stages[0].thoughts).toHaveLength(1);
    expect(stages[0].title).toBe("核对实时信息来源");
    expect(stageOperationSummary(stages[0].tools)).toBe("已完成 2 项工具查找");
    expect(stages[0].title).not.toContain("天气");
    expect(stages.every(stage => !stage.planned)).toBe(true);
  });
  it("真实计划标题跨生成保持阶段，计划更新只影响后续记录", () => {
    const stages = executionStages([
      { kind: "text", text: "第一步", streamId: "1", stageTitle: "核对条件" },
      { ...discovery("a"), streamId: "2", stageTitle: "核对条件" },
      { kind: "text", text: "第二步", streamId: "2", stageTitle: "整理路线" },
    ]);
    expect(stages.map(stage => stage.title)).toEqual(["核对条件", "整理路线"]);
    expect(stages[0].parts).toHaveLength(2);
  });
  it("未知和失败状态不被说明文字或标题伪装为完成", () => {
    const stages = executionStages([discovery("a", "in_progress"), discovery("b", "failed")]);
    expect(stageOperationSummary(stages[0].tools)).toBe("1 项进行中 · 1 项失败");
    expect(stageOperationSummary([])).toBe("");
    expect(stageOperationSummary([{ toolCallId: "web", title: "搜索南京天气", kind: "web_search", status: "completed", content: [] }])).toBe("已完成 1 项操作");
  });
  it("当前阶段、旧阶段并行操作和等待状态分别按实际事件展示", () => {
    const [stage] = executionStages([discovery("a", "in_progress")]);
    expect(executionStageState(stage, true, true, true)).toBe("等待确认");
    expect(executionStageState(stage, false, true)).toBe("进行中");
    expect(executionStageState(stage, false, false)).toBe("未记录完成");
    const [failed] = executionStages([discovery("b", "failed")]);
    expect(executionStageState(failed, false, false)).toBe("含失败记录");
  });
});
