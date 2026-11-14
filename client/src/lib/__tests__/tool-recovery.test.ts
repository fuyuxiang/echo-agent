import { describe, expect, it } from "vitest";
import { findToolRecoveries } from "../tool-recovery";
import type { MessagePart, ToolCallView } from "@/stores/session-store";

const call = (id: string, status: ToolCallView["status"], input: unknown = {command:"fetch-weather",cwd:"/work"}, overrides: Partial<ToolCallView> = {}, streamId?: string): MessagePart => ({
  kind:"tool_call",streamId,toolCall:{toolCallId:id,toolName:"bash",kind:"execute",title:"查询天气",status,content:[],rawInput:input,...overrides},
});

describe("verified tool recovery", () => {
  it.each(["result_unknown", "dispatched_unverified"])("%s 的重试回执不能证明原操作已恢复", status => {
    const identity = { toolName: "computer_click", kind: "other", title: "点击按钮" };
    expect(findToolRecoveries([
      call("failure", "failed", { x: 1, y: 2 }, { ...identity, finishedAt: 100 }),
      call("retry", "completed", { x: 1, y: 2 }, { ...identity, startedAt: 200, content: [{ type: "text", text: JSON.stringify({ status }) }] }),
    ])).toEqual([]);
  });

  it("相同操作的后续重试成功才恢复失败，忽略 JSON 对象的键顺序", () => {
    const parts = [call("failed","failed",undefined,{finishedAt:2_000}),
      call("retry","completed",{cwd:"/work",command:"fetch-weather"},{startedAt:3_000})];
    expect(findToolRecoveries(parts)).toEqual([{failedToolCallId:"failed",successfulToolCallId:"retry"}]);
  });
  it("保留多次失败记录并分别关联到成功重试", () => {
    const parts = [call("first","failed",undefined,{finishedAt:1_000}),call("second","failed",undefined,{finishedAt:2_000}),call("retry","completed",undefined,{startedAt:3_000})];
    expect(findToolRecoveries(parts)).toHaveLength(2);
  });
  it("无时间戳的持久记录可使用不同模型生成轮次证明先后关系", () => {
    expect(findToolRecoveries([call("failed","failed",undefined,{},"generation-1"),call("retry","completed",undefined,{},"generation-2")])).toHaveLength(1);
  });
  it.each([
    ["另一条命令",{command:"list-files",cwd:"/work"},{},{}],
    ["另一个目录",{command:"fetch-weather",cwd:"/other"},{},{}],
    ["不同工具名称",{command:"fetch-weather",cwd:"/work"},{},{toolName:"another-tool"}],
    ["缺少原始参数",null,{},{}],
    ["截断的参数",{truncated:true,preview:"fetch"},{},{}],
    ["并行成功发生在失败之前",{command:"fetch-weather",cwd:"/work"},{finishedAt:4_000},{startedAt:3_000}],
  ] as const)("%s不能解除告警", (_label,input,failedOverrides,successOverrides) => {
    expect(findToolRecoveries([call("failed","failed",undefined,{finishedAt:2_000,...failedOverrides}),call("success","completed",input,{startedAt:3_000,...successOverrides})])).toEqual([]);
  });
  it("同轮并行和没有顺序证据的旧记录均不推测为已恢复", () => {
    for (const streamId of [undefined,"parallel-generation"]) {
      expect(findToolRecoveries([call("failed","failed",undefined,{},streamId),call("success","completed",undefined,{},streamId)])).toEqual([]);
    }
  });
  it("先成功后失败不会倒过来算恢复", () => {
    expect(findToolRecoveries([call("success","completed",undefined,{startedAt:1_000}),call("failed","failed",undefined,{finishedAt:2_000})])).toEqual([]);
  });
  it("缺少工具身份和有效参数时，同名通用卡片不是恢复证据", () => {
    expect(findToolRecoveries([call("failed","failed",{},{toolName:undefined,kind:"other",finishedAt:2_000}),call("success","completed",{},{toolName:undefined,kind:"other",startedAt:3_000})])).toEqual([]);
  });
  it("数组与对象结构不混淆，循环或复杂参数安全降级", () => {
    const circular: Record<string,unknown> = {}; circular.self = circular;
    for (const [first,second] of [[{a:1},[["a",1]]],[circular,circular]]) {
      expect(findToolRecoveries([call("failed","failed",first,{finishedAt:2_000}),call("success","completed",second,{startedAt:3_000})])).toEqual([]);
    }
  });
});
