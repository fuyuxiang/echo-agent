import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, waitFor, act } from "@testing-library/react";
import { MemoryRouter } from "react-router";
import { Overview } from "./Overview";
import * as api from "../lib/api";
import { useAuthStore } from "../stores/auth";

const HEALTH = { status: "degraded", active_sessions: 4, ws_clients: 2 };
const CHANNELS = {
  channels: [
    { name: "feishu", enabled: true, running: true },
    { name: "wecom", enabled: true, running: false },
  ],
};
const TASKS = {
  tasks: [{ status: "running" }, { status: "running" }, { status: "queued" }],
  total: 3,
};

function mockApi() {
  return vi.spyOn(api, "apiFetch").mockImplementation(async (path: string) => {
    if (path === "/health") return HEALTH as never;
    if (path === "/channels") return CHANNELS as never;
    if (path.startsWith("/tasks")) return TASKS as never;
    if (path === "/memory/stats") return { total: 42 } as never;
    return {} as never;
  });
}

function renderOverview() {
  render(<MemoryRouter><Overview /></MemoryRouter>);
}

beforeEach(() => {
  // 不给 token,useWsSubscribe 就不会真的建连接。
  useAuthStore.setState({ token: "" });
});

afterEach(() => {
  vi.useRealTimers();
  vi.restoreAllMocks();
});

describe("Overview 页", () => {
  /** 指标卡是「数字 + 标签」两个相邻节点,按标签定位再取同卡里的数字。 */
  function metric(label: string): string {
    const card = screen.getByText(label).closest("div")?.parentElement;
    return card?.querySelector(".text-2xl")?.textContent ?? "";
  }

  it("指标卡读后端真实字段", async () => {
    mockApi();
    renderOverview();

    await waitFor(() => expect(screen.getByText("degraded")).toBeInTheDocument());
    expect(metric("活跃会话")).toBe("4");
    expect(metric("CLI 客户端")).toBe("2");
    expect(metric("记忆条数")).toBe("42");
    // 通道在线数由 running 统计,两个通道里只有一个在线。
    expect(metric("通道在线")).toBe("1");
    expect(metric("运行中任务")).toBe("2");
  });

  it("看板摘要按状态计数", async () => {
    mockApi();
    renderOverview();

    await waitFor(() => expect(screen.getByText("执行中: 2")).toBeInTheDocument());
    expect(screen.getByText("排队中: 1")).toBeInTheDocument();
    expect(screen.getByText("收件箱: 0")).toBeInTheDocument();
  });

  it("响应缺少 tasks 数组时不崩页", async () => {
    vi.spyOn(api, "apiFetch").mockImplementation(async (path: string) => {
      if (path === "/health") return HEALTH as never;
      if (path.startsWith("/tasks")) return {} as never;
      return { channels: [] } as never;
    });
    renderOverview();

    await waitFor(() => expect(screen.getByText("执行中: 0")).toBeInTheDocument());
  });

  it("定时轮询刷新四个数据源,首屏数字不再冻结", async () => {
    vi.useFakeTimers();
    const spy = mockApi();
    renderOverview();

    // Flush the immediately-resolved initial requests inside act. RTL waitFor
    // cannot drive its polling timer while this test intentionally owns fake
    // timers, and Vitest's waitFor is not wrapped in React.act.
    await act(async () => {
      await Promise.resolve();
    });
    expect(spy).toHaveBeenCalledTimes(4);
    const before = spy.mock.calls.length;

    await act(async () => {
      await vi.advanceTimersByTimeAsync(30_000);
    });

    const added = spy.mock.calls.slice(before).map((c) => c[0]);
    expect(added).toContain("/health");
    expect(added).toContain("/channels");
    expect(added).toContain("/memory/stats");
    expect(added.some((p) => String(p).startsWith("/tasks"))).toBe(true);
  });
});
