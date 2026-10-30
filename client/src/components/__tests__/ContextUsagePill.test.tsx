import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { ContextUsagePill } from "../ContextUsagePill";
import { agentSessionInfo, agentSessionUsage } from "@/lib/agent-client";
import { estimateSendCost } from "@/lib/token-estimate";
vi.mock("@/lib/agent-client", () => ({ agentSessionInfo: vi.fn(), agentSessionUsage: vi.fn(async () => null) }));
const snapshot = (usagePct: number) => ({ context: { used: usagePct * 1000, total: 100000, usagePct, messageTokens: 100, toolDefinitionsTokens: 100, systemPromptTokens: 100 } });

describe("context disclosure", () => {
  beforeEach(() => vi.mocked(agentSessionInfo).mockReset());
  it("无快照时仍可按需检查输入估算，明确非计费结果", () => {
    render(<ContextUsagePill estimate={estimateSendCost("检查任务交付")} />);
    const trigger = screen.getByRole("button", {name:"上下文与用量详情"});
    fireEvent.click(trigger);
    expect(screen.getByRole("dialog",{name:"上下文详情"})).toHaveTextContent("仅为本地估算，非计费结果");
    fireEvent.keyDown(document,{key:"Escape"});
    expect(screen.queryByRole("dialog")).toBeNull(); expect(trigger).toHaveFocus();
  });
  it("正常容量也显示进度环，达到阈值时额外显示占用数字", async () => {
    vi.mocked(agentSessionInfo).mockResolvedValue(snapshot(30) as never);
    const {rerender} = render(<ContextUsagePill sessionId="a" />);
    const trigger = await screen.findByRole("button",{name:"上下文已使用 30%"});
    expect(trigger.querySelector(".context-usage__ring-progress")).toBeInTheDocument();
    expect(screen.queryByText("30%")).toBeNull();
    vi.mocked(agentSessionInfo).mockResolvedValue(snapshot(85) as never);
    rerender(<ContextUsagePill sessionId="b" />);
    await screen.findByRole("button",{name:"上下文已使用 85%"});
    expect(screen.getByText("85%")).toBeVisible();
  });
  it("迟到的旧会话快照不会覆盖新会话", async () => {
    let resolveA!: (value: never) => void;
    vi.mocked(agentSessionInfo).mockImplementationOnce(() => new Promise(resolve => { resolveA = resolve; }));
    const {rerender} = render(<ContextUsagePill sessionId="a" />);
    vi.mocked(agentSessionInfo).mockResolvedValue(snapshot(70) as never);
    rerender(<ContextUsagePill sessionId="b" />);
    await screen.findByRole("button",{name:"上下文已使用 70%"});
    await act(async () => resolveA(snapshot(95) as never));
    await waitFor(() => expect(screen.getByRole("button",{name:"上下文已使用 70%"})).toBeVisible());
    expect(screen.queryByText("95%")).toBeNull();
  });
});

describe("live context capacity", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    vi.mocked(agentSessionInfo).mockReset().mockResolvedValue(snapshot(30) as never);
    vi.mocked(agentSessionUsage).mockReset().mockResolvedValue({ inputTokens: 100, cachedReadTokens: 80 } as never);
  });
  afterEach(() => {
    cleanup();
    vi.useRealTimers();
    vi.restoreAllMocks();
  });
  const tick = async (ms = 0) => { await act(async () => { await vi.advanceTimersByTimeAsync(ms); }); };
  const ring = (pct: number) => screen.getByRole("button", { name: `上下文已使用 ${pct}%` });

  it("不点击、消息条数不变时，运行中的图标也自动更新；压缩后占用可以下降", async () => {
    render(<ContextUsagePill sessionId="a" active onRefreshSignal={2} />);
    await tick();
    expect(ring(30)).toBeVisible();
    vi.mocked(agentSessionInfo).mockResolvedValue(snapshot(98) as never);
    await tick(3_000);
    expect(ring(98)).toHaveClass("context-usage__pill--attention");
    expect(screen.getByText("98%")).toBeVisible();
    vi.mocked(agentSessionInfo).mockResolvedValue(snapshot(20) as never);
    await tick(3_000);
    expect(ring(20)).not.toHaveClass("context-usage__pill--attention");
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(agentSessionUsage).not.toHaveBeenCalled();
  });

  it("回合结束立即读取最终快照，空闲时停止轮询", async () => {
    const view = render(<ContextUsagePill sessionId="a" active onRefreshSignal={2} />);
    await tick();
    vi.mocked(agentSessionInfo).mockResolvedValue(snapshot(75) as never);
    view.rerender(<ContextUsagePill sessionId="a" active={false} onRefreshSignal={2} />);
    await tick();
    expect(ring(75)).toBeVisible();
    expect(agentSessionInfo).toHaveBeenCalledTimes(2);
    await tick(12_000);
    expect(agentSessionInfo).toHaveBeenCalledTimes(2);
  });

  it("用量/模型加载完成信号可以刷新，不因普通重渲染重复请求", async () => {
    const view = render(<ContextUsagePill sessionId="a" onRefreshSignal="loading" />);
    await tick();
    view.rerender(<ContextUsagePill sessionId="a" onRefreshSignal="loading" />);
    await tick();
    expect(agentSessionInfo).toHaveBeenCalledTimes(1);
    vi.mocked(agentSessionInfo).mockResolvedValue(snapshot(70) as never);
    view.rerender(<ContextUsagePill sessionId="a" onRefreshSignal="loaded" />);
    await tick();
    expect(ring(70)).toBeVisible();
    expect(agentSessionInfo).toHaveBeenCalledTimes(2);
  });

  it("弹层用量接口慢不会阻塞容量刷新，弹层打开时也持续同步", async () => {
    vi.mocked(agentSessionUsage).mockReturnValue(new Promise(() => {}));
    render(<ContextUsagePill sessionId="a" />);
    await tick();
    fireEvent.click(ring(30));
    await tick();
    vi.mocked(agentSessionInfo).mockResolvedValue(snapshot(90) as never);
    await tick(3_000);
    expect(ring(90)).toBeVisible();
    expect(screen.getByRole("dialog")).toHaveTextContent("90.0K/100.0K（90%）");
    expect(agentSessionUsage).toHaveBeenCalledTimes(1);
  });

  it("慢请求不并发堆积，并在请求期间收到完成信号后补读最终值", async () => {
    let finish!: (value: never) => void;
    vi.mocked(agentSessionInfo).mockImplementationOnce(() => new Promise(resolve => { finish = resolve; }));
    const view = render(<ContextUsagePill sessionId="a" active />);
    await tick(9_000);
    expect(agentSessionInfo).toHaveBeenCalledTimes(1);
    view.rerender(<ContextUsagePill sessionId="a" active={false} />);
    vi.mocked(agentSessionInfo).mockResolvedValue(snapshot(95) as never);
    await act(async () => finish(snapshot(40) as never));
    await tick();
    expect(ring(95)).toBeVisible();
    expect(agentSessionInfo).toHaveBeenCalledTimes(2);
  });

  it("窗口隐藏时暂停读取，恢复可见和重新聚焦时立即同步", async () => {
    let visibility = "visible";
    vi.spyOn(document, "visibilityState", "get").mockImplementation(() => visibility as DocumentVisibilityState);
    render(<ContextUsagePill sessionId="a" active />);
    await tick();
    visibility = "hidden";
    fireEvent(document, new Event("visibilitychange"));
    await tick(9_000);
    expect(agentSessionInfo).toHaveBeenCalledTimes(1);
    vi.mocked(agentSessionInfo).mockResolvedValue(snapshot(88) as never);
    visibility = "visible";
    fireEvent(document, new Event("visibilitychange"));
    await tick();
    expect(ring(88)).toBeVisible();
    vi.mocked(agentSessionInfo).mockResolvedValue(snapshot(92) as never);
    fireEvent(window, new Event("focus"));
    await tick();
    expect(ring(92)).toBeVisible();
  });

  it("暂时失败保留已知容量并自动重试，未加载会话后续可恢复", async () => {
    vi.mocked(agentSessionInfo).mockResolvedValueOnce({} as never);
    render(<ContextUsagePill sessionId="a" active />);
    await tick();
    expect(screen.queryByRole("button")).toBeNull();
    await tick(3_000);
    expect(ring(30)).toBeVisible();
    vi.mocked(agentSessionInfo).mockRejectedValueOnce(new Error("temporarily unavailable"));
    await tick(3_000);
    expect(ring(30)).toBeVisible();
    vi.mocked(agentSessionInfo).mockResolvedValue(snapshot(65) as never);
    await tick(3_000);
    expect(ring(65)).toBeVisible();
  });

  it("切换会话和卸载会清理轮询与监听，迟到的响应也不会覆盖新会话", async () => {
    let finish!: (value: never) => void;
    vi.mocked(agentSessionInfo).mockImplementationOnce(() => new Promise(resolve => { finish = resolve; }));
    const view = render(<ContextUsagePill sessionId="a" active />);
    vi.mocked(agentSessionInfo).mockResolvedValue(snapshot(60) as never);
    view.rerender(<ContextUsagePill sessionId="b" active />);
    await tick();
    await act(async () => finish(snapshot(99) as never));
    expect(ring(60)).toBeVisible();
    await tick(3_000);
    expect(vi.mocked(agentSessionInfo).mock.calls.map(([id]) => id)).toEqual(["a", "b", "b"]);
    view.unmount();
    await tick(6_000);
    fireEvent(window, new Event("focus"));
    fireEvent(document, new Event("visibilitychange"));
    expect(agentSessionInfo).toHaveBeenCalledTimes(3);
  });
});
