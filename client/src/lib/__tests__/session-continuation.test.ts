import { describe, expect, it } from "vitest";
import { continuationAnchor, continuationTargets, inheritedMessageIds } from "../session-continuation";
import type { ChatMessage } from "@/stores/session-store";
import type { SessionContinuation } from "../types";
const messages: ChatMessage[] = [0,1,2].flatMap(index => [
  {id:`u${index}`, role:"user", complete:true, promptIndex:index, promptId:`p${index}`, parts:[{kind:"text",text:`问题${index}`}]},
  {id:`a${index}`, role:"assistant", complete:true, promptId:`p${index}`, parts:[{kind:"text",text:`回复${index}`}]},
]);
const origin: SessionContinuation = {parentSessionId:"source",parentTitle:"原对话",promptIndex:1,promptId:"p1",inheritedTurnCount:2,temporaryTitle:"新方向",createdAt:"2026-11-03T00:00:00Z"};
describe("conversation continuation boundaries", () => {
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
});
