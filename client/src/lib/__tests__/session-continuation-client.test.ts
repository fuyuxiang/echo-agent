import { beforeEach, describe, expect, it, vi } from "vitest";
vi.mock("@tauri-apps/api/core", () => ({invoke:vi.fn()}));
vi.mock("@tauri-apps/api/event", () => ({listen:vi.fn()}));
import { invoke } from "@tauri-apps/api/core";
import { sessionContinue } from "../agent-client";
import { useContinuationStore } from "@/stores/continuation-store";
describe("continuation request lifecycle", () => {
  beforeEach(() => {vi.mocked(invoke).mockReset();useContinuationStore.setState({pending:{}});});
  it("菜单和命令同时触发时只复制一次，并在完成后释放发送锁", async () => {
    let finish!:(value:unknown)=>void;
    vi.mocked(invoke).mockImplementationOnce(() => new Promise(resolve=>{finish=resolve;}));
    const first = sessionContinue("source","/workspace",{requestId:"request",targetPromptIndex:1,sourcePromptId:"p1"});
    expect(useContinuationStore.getState().pending.source).toBe(true);
    const second = sessionContinue("source","/workspace",{requestId:"another"});
    expect(second).toBe(first);
    expect(invoke).toHaveBeenCalledOnce();
    expect(invoke).toHaveBeenCalledWith("session_continue",{sessionId:"source",cwd:"/workspace",requestId:"request",targetPromptIndex:1,sourcePromptId:"p1"});
    finish({session:{sessionId:"child"},warnings:[]});
    await first;
    expect(useContinuationStore.getState().pending.source).toBeUndefined();
  });
  it("失败释放发送锁，重试保留原请求标识和历史范围", async () => {
    vi.mocked(invoke).mockRejectedValueOnce(new Error("offline")).mockResolvedValueOnce({session:{sessionId:"child"},warnings:[]});
    const request = {requestId:"same-request",targetPromptIndex:0};
    await expect(sessionContinue("source","/workspace",request)).rejects.toThrow("offline");
    expect(useContinuationStore.getState().pending.source).toBeUndefined();
    await sessionContinue("source","/workspace",request);
    expect(vi.mocked(invoke).mock.calls[0]).toEqual(vi.mocked(invoke).mock.calls[1]);
  });
  it("刷新来源持有共享状态时阻止其他入口开始接续，并保留刷新状态", async () => {
    useContinuationStore.getState().setPending("source",true);
    await expect(sessionContinue("source","/workspace",{requestId:"request"})).rejects.toThrow("正在更新对话历史");
    expect(invoke).not.toHaveBeenCalled();
    expect(useContinuationStore.getState().pending.source).toBe(true);
  });
});
