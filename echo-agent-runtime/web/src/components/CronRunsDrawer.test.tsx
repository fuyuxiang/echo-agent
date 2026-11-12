import { afterEach, expect, it, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import { CronRunsDrawer } from "./CronRunsDrawer";
import * as api from "../lib/api";
import type { CronJob } from "../pages/Cron";

afterEach(() => vi.restoreAllMocks());

it("暂时拒收的日报明确展示自动恢复与时间", async () => {
  vi.spyOn(api, "apiFetch").mockResolvedValue({ runs: [{ run_id: "r3", ts: 1700000000000,
    status: "delivery_pending", run_count: 1, generation_status: "ready", delivery_status: "awaiting_context",
    deliveries: [{ recovery_action: "retry_later", retry_at_ms: 1794502800000 }] }] });
  render(<CronRunsDrawer job={{ id: "j1", name: "日报" } as CronJob} onClose={() => {}} />);
  expect(await screen.findByRole("status")).toHaveTextContent("系统将在冷却后自动重试");
  expect(screen.getByRole("status")).toHaveTextContent("计划重试时间");
});

it("分别展示报告生成和投递状态，并给出恢复动作和下载入口", async () => {
  vi.spyOn(api, "apiFetch").mockResolvedValue({ runs: [{ run_id: "r1", ts: 1700000000000,
    status: "delivery_pending", run_count: 1, error: "ret=-2: prepare failed", generation_status: "ready",
    report_file: "reports/daily.md", delivery_status: "awaiting_context", deliveries: [{ recovery_action: "send_message" }] }] });
  render(<CronRunsDrawer job={{ id: "j1", name: "日报" } as CronJob} onClose={() => {}} />);
  expect(await screen.findByText("报告生成: 已生成并保存")).toBeInTheDocument();
  expect(screen.getByRole("status")).toHaveTextContent("恢复会话后会自动补发，无需重新运行任务");
  expect(screen.getByRole("button", { name: "下载已保存的报告" })).toBeInTheDocument();
  expect(screen.getByText("错误").closest("details")).not.toHaveAttribute("open");
});

it("未完成的草稿不显示完整报告下载", async () => {
  vi.spyOn(api, "apiFetch").mockResolvedValue({ runs: [{ run_id: "r2", ts: 1700000000000,
    status: "incomplete", run_count: 2, generation_status: "incomplete" }] });
  render(<CronRunsDrawer job={{ id: "j1", name: "日报" } as CronJob} onClose={() => {}} />);
  expect(await screen.findByText("报告生成: 未完成，草稿已保存")).toBeInTheDocument();
  expect(screen.queryByRole("button", { name: "下载已保存的报告" })).not.toBeInTheDocument();
});
