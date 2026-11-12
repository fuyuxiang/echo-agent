import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { Channels } from "./Channels";
import { useToastStore } from "../stores/toast";

const mocks = vi.hoisted(() => ({ fetch: vi.fn(), refetch: vi.fn(), channels: [] as {
  name: string; enabled: boolean; running: boolean; enabled_managed?: boolean;
  delivery_health?: string;
  delivery_recovery?: { pending_conversations: number; retry_at_ms: number; retry_attempts: number };
}[] }));
vi.mock("../lib/api", () => ({ apiFetch: mocks.fetch }));
vi.mock("../hooks/use-api", () => ({ useApi: () => ({
  data: { channels: mocks.channels },
  loading: false, error: null, refetch: mocks.refetch,
}) }));
vi.mock("../hooks/use-ws", () => ({ useWsSubscribe: () => {} }));
vi.mock("../stores/capabilities", () => ({ useIsAdmin: () => true }));
vi.mock("../components/DeliveryRecovery", () => ({ DeliveryRecovery: () => null }));

beforeEach(() => {
  mocks.fetch.mockReset();
  mocks.refetch.mockReset();
  mocks.channels = [{ name: "telegram", enabled: true, running: true, enabled_managed: true }];
  useToastStore.setState({ toasts: [] });
});

afterEach(() => vi.useRealTimers());

describe("渠道即时停止", () => {
  it("启动设置锁定启用开关时仍能直接停止，无须先保存配置", async () => {
    mocks.fetch.mockResolvedValue({ success: true });
    render(<Channels />);
    expect(screen.getByRole("switch", { name: "停用通道 telegram" })).toBeDisabled();
    fireEvent.click(screen.getByRole("button", { name: "停止通道 telegram" }));
    await waitFor(() => expect(mocks.refetch).toHaveBeenCalledOnce());
    expect(mocks.fetch).toHaveBeenCalledExactlyOnceWith("/channels/telegram/stop", { method: "POST" });
    expect(useToastStore.getState().toasts.at(-1)?.message).toContain("通道已停止");
  });

  it("停止失败时显示失败原因并恢复按钮，不提示成功", async () => {
    mocks.fetch.mockRejectedValue(new Error("connection did not close"));
    render(<Channels />);
    const stop = screen.getByRole("button", { name: "停止通道 telegram" });
    fireEvent.click(stop);
    await waitFor(() => expect(useToastStore.getState().toasts.at(-1)?.kind).toBe("error"));
    expect(useToastStore.getState().toasts.at(-1)?.message).toContain("connection did not close");
    expect(stop).toBeEnabled();
    expect(mocks.refetch).not.toHaveBeenCalled();
  });
});

describe("通道投递状态", () => {
  it.each(["no_context", "login_required", "cooling_down", "retrying"])("%s 时显示投递待恢复，并保留恢复提示", health => {
    mocks.channels = [{ name: "weixin", enabled: true, running: true, delivery_health: health,
      delivery_recovery: { pending_conversations: 1, retry_at_ms: 1794502800000, retry_attempts: 1 } }];
    render(<Channels />);
    expect(screen.getByRole("status")).toHaveTextContent("投递待恢复");
    expect(screen.getByRole("status")).toHaveClass("bg-yellow-100");
    expect(screen.queryByText("在线")).not.toBeInTheDocument();
    if (health === "cooling_down") expect(screen.getByText(/计划重试时间/)).toBeInTheDocument();
    else expect(screen.queryByText(/计划重试时间/)).not.toBeInTheDocument();
  });

  it("自动恢复后更新为在线，并清除旧的重试时间", () => {
    mocks.channels = [{ name: "weixin", enabled: true, running: true, delivery_health: "cooling_down" }];
    const view = render(<Channels />);
    expect(screen.getByRole("status")).toHaveTextContent("投递待恢复");
    mocks.channels = [{ name: "weixin", enabled: true, running: true, delivery_health: "ready" }];
    view.rerender(<Channels />);
    expect(screen.getByRole("status")).toHaveTextContent("在线");
    expect(screen.getByRole("status")).toHaveClass("bg-green-100");
    expect(screen.queryByText(/计划重试时间/)).not.toBeInTheDocument();
  });

  it("已启用但断线的通道仍定时刷新，离开页面停止轮询", () => {
    vi.useFakeTimers();
    mocks.channels = [{ name: "weixin", enabled: true, running: false }];
    const view = render(<Channels />);
    expect(screen.getByRole("status")).toHaveTextContent("未连接");
    act(() => vi.advanceTimersByTime(15000));
    expect(mocks.refetch).toHaveBeenCalledOnce();
    view.unmount();
    act(() => vi.advanceTimersByTime(15000));
    expect(mocks.refetch).toHaveBeenCalledOnce();
  });

  it("已停用且不在运行的通道不发起轮询", () => {
    vi.useFakeTimers();
    mocks.channels = [{ name: "weixin", enabled: false, running: false }];
    render(<Channels />);
    expect(screen.getByRole("status")).toHaveTextContent("离线");
    act(() => vi.advanceTimersByTime(15000));
    expect(mocks.refetch).not.toHaveBeenCalled();
  });
});
