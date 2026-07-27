import { describe, it, expect, vi, afterEach } from "vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import { Analytics } from "./Analytics";
import * as api from "../lib/api";

const USAGE = {
  usage: [
    { date: "2026-07-26", input_tokens: 100, output_tokens: 50, cost_usd: 0.0123 },
    { date: "2026-07-27", input_tokens: 200, output_tokens: 80, cost_usd: 0.0456 },
  ],
};

const CHANNELS = {
  channels: [{ channel: "feishu", input_tokens: 300, output_tokens: 130, cost_usd: 0.0579 }],
};

function mockApi() {
  return vi.spyOn(api, "apiFetch").mockImplementation(async (path: string) => {
    if (path.startsWith("/analytics/tokens")) return USAGE as never;
    if (path.startsWith("/analytics/channels")) return CHANNELS as never;
    return {} as never;
  });
}

afterEach(() => {
  vi.restoreAllMocks();
});

describe("Analytics 页", () => {
  it("默认取 7 天,同时拉 token 与渠道两个端点", async () => {
    const spy = mockApi();
    render(<Analytics />);

    await waitFor(() => expect(spy).toHaveBeenCalledWith("/analytics/tokens?days=7"));
    // 渠道归因端点此前完全没被前端使用。
    expect(spy).toHaveBeenCalledWith("/analytics/channels?days=7");
    expect(screen.getByRole("button", { name: "7天" })).toHaveAttribute("aria-pressed", "true");
  });

  it("汇总区间总成本", async () => {
    mockApi();
    render(<Analytics />);

    expect(await screen.findByText("区间总成本：$0.0579")).toBeInTheDocument();
  });

  it("切时间范围时两个端点都跟着变", async () => {
    const spy = mockApi();
    render(<Analytics />);

    await waitFor(() => expect(spy).toHaveBeenCalledWith("/analytics/tokens?days=7"));
    fireEvent.click(screen.getByRole("button", { name: "30天" }));

    await waitFor(() => expect(spy).toHaveBeenCalledWith("/analytics/tokens?days=30"));
    expect(spy).toHaveBeenCalledWith("/analytics/channels?days=30");
  });

  it("渲染成本趋势区块 —— cost_usd 之前只在类型里声明,从没画出来", async () => {
    mockApi();
    render(<Analytics />);

    expect(await screen.findByText("成本趋势")).toBeInTheDocument();
    expect(screen.getByText("渠道成本归因")).toBeInTheDocument();
  });

  it("token 端点失败时显示错误", async () => {
    vi.spyOn(api, "apiFetch").mockImplementation(async (path: string) => {
      if (path.startsWith("/analytics/tokens")) throw new Error("boom");
      return CHANNELS as never;
    });
    render(<Analytics />);

    expect(await screen.findByText(/加载失败：boom/)).toBeInTheDocument();
  });

  it("渠道端点单独失败只影响该区块", async () => {
    vi.spyOn(api, "apiFetch").mockImplementation(async (path: string) => {
      if (path.startsWith("/analytics/channels")) throw new Error("no ledger");
      return USAGE as never;
    });
    render(<Analytics />);

    expect(await screen.findByText(/加载失败：no ledger/)).toBeInTheDocument();
    // 上面两张图仍然要在。
    expect(screen.getByText("Token 消耗趋势")).toBeInTheDocument();
    expect(screen.getByText("区间总成本：$0.0579")).toBeInTheDocument();
  });

  it("渠道无数据时给空态而不是空白图", async () => {
    vi.spyOn(api, "apiFetch").mockImplementation(async (path: string) => {
      if (path.startsWith("/analytics/channels")) return { channels: [] } as never;
      return USAGE as never;
    });
    render(<Analytics />);

    expect(await screen.findByText("暂无数据")).toBeInTheDocument();
  });
});
