import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { Channels } from "./Channels";
import { useToastStore } from "../stores/toast";

const mocks = vi.hoisted(() => ({ fetch: vi.fn(), refetch: vi.fn() }));
vi.mock("../lib/api", () => ({ apiFetch: mocks.fetch }));
vi.mock("../hooks/use-api", () => ({ useApi: () => ({
  data: { channels: [{ name: "telegram", enabled: true, running: true, enabled_managed: true }] },
  loading: false, error: null, refetch: mocks.refetch,
}) }));
vi.mock("../hooks/use-ws", () => ({ useWsSubscribe: () => {} }));
vi.mock("../stores/capabilities", () => ({ useIsAdmin: () => true }));
vi.mock("../components/DeliveryRecovery", () => ({ DeliveryRecovery: () => null }));

beforeEach(() => {
  mocks.fetch.mockReset();
  mocks.refetch.mockReset();
  useToastStore.setState({ toasts: [] });
});

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
