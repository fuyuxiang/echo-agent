import { describe, expect, it } from "vitest";
import { continuationAnchor, continuationTargets, inheritedMessageIds, latestContinuationTarget } from "../session-continuation";
import type { ChatMessage } from "@/stores/session-store";
import type { SessionContinuation } from "../types";
const messages: ChatMessage[] = [0,1,2].flatMap(index => [
  {id:`u${index}`, role:"user", complete:true, promptIndex:index, promptId:`p${index}`, parts:[{kind:"text",text:`问题${index}`}]},
  {id:`a${index}`, role:"assistant", complete:true, promptId:`p${index}`, parts:[{kind:"text",text:`回复${index}`}]},
]);
const origin: SessionContinuation = {parentSessionId:"source",parentTitle:"原对话",promptIndex:1,promptId:"p1",inheritedTurnCount:2,temporaryTitle:"新方向",createdAt:"2026-11-03T00:00:00Z"};
describe("conversation continuation boundaries", () => {
  it.each([{ parts: [] }, { parts: [{ kind: "thought" as const, text: "执行前模型连接失败" }] }])("默认接续保留没有正文的最新失败请求：$parts", ({ parts }) => {
    const failed: ChatMessage[] = [...messages,
      { id: "u3", role: "user", complete: true, promptIndex: 3, promptId: "p3", parts: [{ kind: "text", text: "最新约束" }] },
      { id: "a3", role: "assistant", complete: true, promptIndex: 3, promptId: "p3", stopReason: "error", parts },
    ];
    expect(latestContinuationTarget(failed)).toEqual({ promptIndex: 3, promptId: "p3" });
    expect(continuationAnchor(failed, latestContinuationTarget(failed)!)).toBe("a3");
    expect(continuationTargets(failed).has("a3")).toBe(false);
  });
  it("没有可见用户回显的失败后台轮次仍是默认接续检查点", () => {
    const failed: ChatMessage = { id: "bg", role: "assistant", complete: true, promptIndex: 3,
      promptId: "task-completed-t", stopReason: "error", parts: [] };
    expect(latestContinuationTarget([...messages, failed])).toEqual({ promptIndex: 3, promptId: "task-completed-t" });
  });
  it("旧后台轮次缺少检查点时交给原生层选择，不能静默退回较早回复", () => {
    expect(latestContinuationTarget([...messages, { id: "bg", role: "assistant", complete: true,
      promptId: "task-completed-t", parts: [{ kind: "text", text: "补充" }] }])).toBeUndefined();
    expect(latestContinuationTarget(messages)).toEqual({ promptIndex: 2, promptId: "p2" });
  });
  it("从后台轮次接续后，首次新消息和等待回复不被折叠进继承历史", () => {
    const inherited = messages.slice(0, 2);
    const background: ChatMessage = { id: "bg", role: "assistant", complete: true,
      promptIndex: 1, promptId: "task-completed-t", stopReason: "end_turn", parts: [{ kind: "text", text: "补充" }] };
    const next: ChatMessage[] = [
      { id: "new-user", role: "user", complete: true, parts: [{ kind: "text", text: "继续新方向" }] },
      { id: "new-answer", role: "assistant", complete: false, parts: [] },
    ];
    expect([...inheritedMessageIds([...inherited, background, ...next], {
      ...origin, promptId: background.promptId,
    })]).toEqual(["u0", "a0", "bg"]);
  });
  it("中间轮次只继承截至所选回复的消息，后续新消息独立", () => {
    expect([...inheritedMessageIds(messages,origin)]).toEqual(["u0","a0","u1","a1"]);
    expect(continuationTargets(messages).get("a1")).toEqual({promptIndex:1,promptId:"p1"});
  });
  it("只在完整轮次末尾提供入口，不截断执行过程", () => {
    const pending:ChatMessage = {id:"pending",role:"assistant",complete:false,parts:[{kind:"text",text:"正在执行"}]};
    expect(continuationTargets([...messages,pending]).has("a2")).toBe(false);
    expect(continuationTargets([...messages,pending]).has("pending")).toBe(false);
  });
  it("回到原位置使用稳定轮次标识，回溯后不会错误定位到同索引的新内容", () => {
    expect(continuationAnchor(messages,{promptIndex:1,promptId:"p1"})).toBe("a1");
    expect(continuationAnchor(messages,{promptIndex:1,promptId:"deleted-turn"})).toBeUndefined();
  });
  it("旧记录缺少索引时仍可用继承轮数折叠历史", () => {
    expect([...inheritedMessageIds(messages.map(({promptIndex:_,...message})=>message),origin)]).toEqual(["u0","a0","u1","a1"]);
  });
  it("索引与轮次身份不匹配时不提供错误的接续位置", () => {
    const mixed: ChatMessage[] = [...messages,
      { id: "unknown", role: "assistant", complete: true, promptId: "other-native-turn", parts: [{ kind: "text", text: "补充" }] },
      { id: "background", role: "assistant", complete: true, promptId: "task-completed-t", promptIndex: 3, parts: [{ kind: "text", text: "后台结果" }] },
    ];
    expect(continuationTargets(mixed).has("unknown")).toBe(false);
    expect(continuationTargets(mixed).get("background")).toEqual({ promptIndex: 3, promptId: "task-completed-t" });
    expect(continuationAnchor(mixed, { promptIndex: 2, promptId: "task-completed-t" })).toBeUndefined();
  });
});
