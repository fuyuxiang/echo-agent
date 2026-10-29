import { describe, it, expect, beforeEach, vi } from "vitest";
import { act, render, screen, fireEvent } from "@testing-library/react";
import { UsageQuotaPanel } from "../UsageQuotaPanel";
import { recordUsage, clearUsage, loadQuotaConfig, saveQuotaConfig, todayKey, type UsageRecord } from "@/lib/usage-quota";

describe("UsageQuotaPanel", () => {
  beforeEach(() => {
    clearUsage();
    window.localStorage.removeItem("echoagent.quota");
  });

  it("空数据显示 0 token + 0 调用", () => {
    render(<UsageQuotaPanel />);
    expect(screen.getByRole("region", { name: "Token 用量与配额" })).toHaveAttribute("tabindex", "0");
    // 空数据时总 Token 显示 0(作为 stat-value)。
    const tokenStat = screen.getByText("总 Token");
    expect(tokenStat.previousElementSibling?.textContent).toBe("0");
    expect(screen.getByText("模型调用")).toBeInTheDocument();
  });

  it("有用量记录 → 显示汇总", () => {
    recordUsage([], { modelId: "gpt-4", promptTokens: 500, completionTokens: 200 });
    render(<UsageQuotaPanel />);
    // totalTokens = 700
    expect(screen.getAllByText("700").length).toBeGreaterThan(0);
    const calls = screen.getByText("模型调用");
    expect(calls.previousElementSibling?.textContent).toBe("1");
  });

  it("切换本月/今日周期", () => {
    render(<UsageQuotaPanel />);
    expect(screen.getByText("今日")).toBeInTheDocument();
    fireEvent.click(screen.getByText("本月"));
    // 重新渲染后仍正常。
    expect(screen.getByText("总 Token")).toBeInTheDocument();
  });

  it("配置配额 → 显示进度条", () => {
    recordUsage([], { modelId: "gpt-4", promptTokens: 800, completionTokens: 200 });
    saveQuotaConfig({ period: "daily", tokenLimit: 1000 });
    render(<UsageQuotaPanel />);
    // 1000 token used = 1000, limit 1000 → pct 100
    expect(screen.getByText(/1,000 \/ 1,000/)).toBeInTheDocument();
  });

  it("切换统计视图不改变已配置限额的计算周期", () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-10-18T12:00:00"));
    try {
      recordUsage([{ date: "2026-10-17", modelId: "gpt-4", promptTokens: 900, completionTokens: 0, ts: new Date("2026-10-17T12:00:00").getTime() }], { modelId: "gpt-4", promptTokens: 100, completionTokens: 0 });
      saveQuotaConfig({ period: "daily", tokenLimit: 500 });
      render(<UsageQuotaPanel />);
      expect(screen.getByText("每日配额")).toBeInTheDocument();
      expect(screen.getByText("100 / 500 (20%)")).toBeInTheDocument();
      fireEvent.click(screen.getByText("本月"));
      expect(screen.getByText("每日配额")).toBeInTheDocument();
      expect(screen.getByText("100 / 500 (20%)")).toBeInTheDocument();
      expect(screen.getByText("仅限制本机手动发送；自动化和其他客户端不受此限制。")).toBeInTheDocument();
    } finally {
      vi.useRealTimers();
    }
  });

  it("按模型分组显示", () => {
    recordUsage([], { modelId: "gpt-4", promptTokens: 100, completionTokens: 50 });
    recordUsage([{
      date: todayKey(),
      modelId: "gpt-4",
      promptTokens: 100,
      completionTokens: 50,
      ts: Date.now(),
    } as UsageRecord], { modelId: "claude", promptTokens: 50, completionTokens: 30 });
    render(<UsageQuotaPanel />);
    expect(screen.getAllByText("gpt-4").length).toBeGreaterThan(0);
    expect(screen.getAllByText("claude").length).toBeGreaterThan(0);
  });

  it("面板打开时用量变化会实时刷新", () => {
    render(<UsageQuotaPanel />);
    act(() => {
      recordUsage([], { modelId: "gpt-4", promptTokens: 90, completionTokens: 10 });
    });
    expect(screen.getAllByText("100").length).toBeGreaterThan(0);
  });

  it("趋势柱标签用紧凑数字，完整数值放在 tooltip", () => {
    recordUsage([], { modelId: "gpt-4", promptTokens: 1_200_000, completionTokens: 34_567 });
    render(<UsageQuotaPanel />);
    // 趋势列只有 ~50px 宽，1,234,567 这类完整数字会被裁掉。
    expect(screen.getByText("1.23M")).toBeInTheDocument();
    expect(
      screen.getByTitle(`${todayKey()}: 1,234,567 Token`),
    ).toBeInTheDocument();
  });

  it("可配置本地模型费率", () => {
    recordUsage([], { modelId: "gpt-4", promptTokens: 1000, completionTokens: 500 });
    render(<UsageQuotaPanel />);
    fireEvent.change(screen.getByLabelText("gpt-4 · 个人 输入费率"), { target: { value: "30" } });
    fireEvent.change(screen.getByLabelText("gpt-4 · 个人 输出费率"), { target: { value: "60" } });
    fireEvent.click(screen.getByText("保存费率"));
    expect(screen.getByText("本地估算费率已保存，历史记录已按新费率重新计算。")).toBeInTheDocument();
    expect(screen.getAllByText("$0.0600").length).toBeGreaterThan(0);
  });

  it("同名内置与组织模型显示来源，费率仍按完整 ID 分别保存", () => {
    const records = recordUsage([], { modelId: "echoagent-ojlab/Echo Reed", promptTokens: 1000, completionTokens: 500 });
    recordUsage(records, { modelId: "organization/Echo Reed", promptTokens: 200, completionTokens: 100 });
    const { container } = render(<UsageQuotaPanel />);
    expect(container.textContent).not.toMatch(/echoagent-ojlab\/|organization\//);
    fireEvent.change(screen.getByLabelText("Echo Reed · 内置 输入费率"), { target: { value: "30" } });
    fireEvent.change(screen.getByLabelText("Echo Reed · 组织 输入费率"), { target: { value: "60" } });
    fireEvent.click(screen.getByText("保存费率"));
    expect(loadQuotaConfig()?.rates).toMatchObject({
      "echoagent-ojlab/Echo Reed": { prompt: 0.03 },
      "organization/Echo Reed": { prompt: 0.06 },
    });
  });
});
