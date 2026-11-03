import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { ConversationOrigin } from "../ConversationContinuation";
import type { SessionSummary } from "@/lib/types";
const child:SessionSummary = {sessionId:"child",title:"新方向",cwd:"/workspace",continuation:{parentSessionId:"source",parentTitle:"原名称",promptIndex:1,promptId:"stable-turn",inheritedTurnCount:2,temporaryTitle:"新方向",createdAt:"2026-11-03T00:00:00Z"}};
describe("continuation provenance", () => {
  it("来源使用父会话当前名称，返回时携带稳定轮次标识", () => {
    const open=vi.fn();
    render(<ConversationOrigin session={child} catalog={[{sessionId:"source",title:"已改名的原对话",cwd:"/workspace"}]} deleted={[]} expanded={false} onToggle={()=>{}} onOpen={open} />);
    fireEvent.click(screen.getByRole("button",{name:"已改名的原对话"}));
    expect(open).toHaveBeenCalledWith("source",{promptIndex:1,promptId:"stable-turn"});
    expect(screen.getByRole("button",{name:/已继承 2 轮/})).toHaveAttribute("aria-expanded","false");
  });
  it("重启后原会话已删除时保留名称和上下文，但不提供无效链接", () => {
    render(<ConversationOrigin session={{...child,parentMissing:true}} catalog={[]} deleted={[]} expanded onToggle={()=>{}} onOpen={vi.fn()} />);
    expect(screen.getByText("原名称")).toBeVisible();
    expect(screen.getByText(/原对话不可用/)).toBeVisible();
    expect(screen.queryByRole("button",{name:"原名称"})).toBeNull();
    expect(screen.getByRole("button",{name:/已继承 2 轮/})).toHaveAttribute("aria-expanded","true");
  });
});
