import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { MessageItem } from "../MessageItem";
import { ThemeProvider } from "../ThemeProvider";
import type { ChatMessage } from "@/stores/session-store";

const failed: ChatMessage = { id: "delivery", role: "assistant", complete: true, stopReason: "end_turn", startedAt: 1000, completedAt: 376000, parts: [
  { kind: "tool_call", toolCall: { toolCallId: "upload-one", kind: "upload_file", title: "上传报告", status: "failed", content: [{ type: "text", text: "Upload transport error" }] } },
  { kind: "text", text: "这里是已经整理好的报告。" },
] };

describe("compact process and actionable result notices", () => {
  it("默认入口中性且只显示耗时，失败计数和原始原因仅展开可见", () => {
    const { container } = render(<ThemeProvider><MessageItem message={failed} streaming={false} /></ThemeProvider>);
    const header = screen.getByRole("button", { name: /执行记录/ });
    expect(header).toHaveAccessibleName(/6分15秒/);
    expect(header).not.toHaveTextContent(/项操作|部分|失败/);
    expect(header).toHaveAttribute("aria-expanded", "false");
    expect(container.querySelector(".execution-process__outcome-detail")).toBeNull();
    expect(screen.getByRole("alert")).toHaveTextContent("文件上传未成功");
    fireEvent.click(header);
    expect(screen.getByText(/1 项失败操作尚未确认恢复/)).toBeVisible();
  });
  it("纯查询的中间异常不干扰答案，失败记录仍保留", () => {
    render(<ThemeProvider><MessageItem message={{ ...failed, parts: [
      { ...failed.parts[0], toolCall: { ...(failed.parts[0] as Extract<ChatMessage["parts"][number], {kind:"tool_call"}>).toolCall, kind: "web_fetch" } } as ChatMessage["parts"][number],
      failed.parts[1],
    ] }} streaming={false} /></ThemeProvider>);
    expect(screen.queryByRole("alert")).toBeNull();
    expect(screen.getByText("这里是已经整理好的报告。")).toBeVisible();
  });
  it("检查并继续防重复提交，查看操作直达原记录，原有重新执行不重复展示", async () => {
    let finish!: (value: boolean) => void;
    const onContinue = vi.fn(() => new Promise<boolean>((resolve) => { finish = resolve; }));
    const onRetry = vi.fn(), onOpen = vi.fn();
    render(<ThemeProvider><MessageItem message={failed} streaming={false} onContinueExecution={onContinue} onRetry={onRetry} onOpenTool={onOpen} /></ThemeProvider>);
    fireEvent.click(screen.getByRole("button", { name: "查看相关操作" }));
    expect(onOpen).toHaveBeenCalledWith(expect.objectContaining({ toolCallId: "upload-one" }));
    const button = screen.getByRole("button", { name: "检查并继续" });
    fireEvent.click(button); fireEvent.click(button);
    expect(onContinue).toHaveBeenCalledTimes(1);
    expect(onContinue.mock.calls[0]).toEqual([expect.stringContaining("先核对文件")]);
    expect(screen.queryByRole("button", { name: "重新执行" })).toBeNull();
    expect(onRetry).not.toHaveBeenCalled();
    await act(async () => finish(false));
    expect(screen.getByText("暂时无法继续，请检查当前任务状态后重试。")).toBeVisible();
    expect(screen.getByRole("button", { name: "检查并继续" })).toBeEnabled();
  });
  it("额度错误只有一条明确提示，查看原因可展开原始记录", async () => {
    const settings = vi.fn();
    render(<ThemeProvider><MessageItem message={{ ...failed, parts: [], stopReason: "error", agentResult: "quota exhausted" }} streaming={false} onOpenModelSettings={settings} /></ThemeProvider>);
    expect(screen.getAllByRole("alert")).toHaveLength(1);
    expect(screen.getByRole("alert")).toHaveTextContent("模型额度已用尽");
    expect(screen.queryByText("quota exhausted")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "模型设置" })); expect(settings).toHaveBeenCalledOnce();
    fireEvent.click(screen.getByRole("button", { name: "查看原因" }));
    await waitFor(() => expect(screen.getByText("quota exhausted")).toBeVisible());
    expect(screen.getAllByRole("alert")).toHaveLength(1);
  });
  it("运行时也默认收起，用户手动展开后不被正式回复抢走控制", () => {
    const message: ChatMessage = { id: "live-default", role: "assistant", complete: false, parts: [{ kind: "thought", text: "分析中" }] };
    const { rerender } = render(<ThemeProvider><MessageItem message={message} streaming /></ThemeProvider>);
    const header = screen.getByRole("button", { name: /正在分析/ });
    expect(header).toHaveAttribute("aria-expanded", "false");
    fireEvent.click(header);
    rerender(<ThemeProvider><MessageItem message={{ ...message, parts: [...message.parts, { kind: "text", text: "正式回复" }] }} streaming /></ThemeProvider>);
    expect(screen.getByRole("button", { name: /正在生成回复/ })).toHaveAttribute("aria-expanded", "true");
  });
  it("历史缺少结束时间不把现在与旧开始时间的差伪装成执行耗时", () => {
    render(<ThemeProvider><MessageItem message={{ ...failed, completedAt: undefined }} streaming={false} /></ThemeProvider>);
    expect(screen.getByRole("button", { name: /执行记录/ }).querySelector(".execution-process__meta")).toBeNull();
  });
  it("缺权限在对应回复说明影响并打开设置，不提供重复执行入口", () => {
    const settings = vi.fn();
    render(<ThemeProvider><MessageItem message={{ ...failed, parts: [], stopReason: "error", agentResult: "需要授予 EchoAgent 屏幕录制权限。" }} streaming={false} onOpenSecuritySettings={settings} onRetry={vi.fn()} /></ThemeProvider>);
    expect(screen.getAllByRole("alert")).toHaveLength(1);
    expect(screen.getByRole("alert")).toHaveTextContent("电脑操作需要系统权限");
    fireEvent.click(screen.getByRole("button", {name:"打开权限设置"}));
    expect(settings).toHaveBeenCalledOnce();
    expect(screen.queryByRole("button", {name:/重试|重新执行|检查并继续/})).toBeNull();
  });

});
