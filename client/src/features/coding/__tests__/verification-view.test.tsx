import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

import type { VerificationRecord } from "../lib/types";

import { VerificationView } from "../panels/VerificationView";

function record(overrides: Partial<VerificationRecord> = {}): VerificationRecord {
  return { id: "test-result", taskId: "task-1", kind: "test", command: "pnpm test", status: "passed", exitCode: 0,
    stdout: "", stderr: "", durationMs: 25, startedAt: "", finishedAt: "", structured: false,
    contentRevision: "source-one", configurationRevision: "config-one", ...overrides };
}

describe("VerificationView", () => {
  it("offers only the expected-failure test run until RED is recorded", async () => {
    const command = { command: "pnpm test", kind: "test" as const, label: "测试", requiresApproval: true };
    const onRunRed = vi.fn();
    render(<VerificationView records={[]} detected={[command]} running={false} hasTask redCheckpoint onRun={vi.fn()} onRunAll={vi.fn()} onRunRed={onRunRed} onOpenOutput={vi.fn()} />);

    expect(screen.getByRole("button", { name: "运行全部验证" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "需要确认测试" })).toBeDisabled();
    await userEvent.click(screen.getByRole("button", { name: "运行 RED · 测试" }));
    expect(onRunRed).toHaveBeenCalledWith(command);
  });
  it("offers normal independent checks without requiring RED or manual review", async () => {
    const onRun = vi.fn();
    const testCommand = { kind: "test" as const, command: "pnpm test", label: "测试" };
    const buildCommand = { kind: "build" as const, command: "pnpm build", label: "构建" };
    render(<VerificationView records={[]} detected={[testCommand, buildCommand]} running={false} hasTask onRun={onRun} onRunAll={vi.fn()} onOpenOutput={vi.fn()} />);
    expect(screen.getByRole("button", { name: "运行全部验证" })).toBeEnabled();
    await userEvent.click(screen.getByRole("button", { name: "测试" }));
    expect(onRun).toHaveBeenCalledWith(testCommand);
    expect(screen.queryByRole("button", { name: /运行 RED/ })).not.toBeInTheDocument();
  });

  it("retains independently executed results and opens the selected real log", async () => {
    const first = record();
    const second = record({ id: "build-result", kind: "build", command: "pnpm build", status: "failed", exitCode: 2 });
    const onOpenOutput = vi.fn();
    render(<VerificationView records={[first, second]} detected={[]} running={false} hasTask onRun={vi.fn()} onRunAll={vi.fn()} onOpenOutput={onOpenOutput} />);
    expect(screen.getByText("pnpm test").closest("button")).toHaveTextContent("通过");
    expect(screen.getByText("pnpm test").closest("button")).toHaveTextContent("退出码 0");
    expect(screen.getByText("pnpm build").closest("button")).toHaveTextContent("失败");
    expect(screen.getByText("pnpm build").closest("button")).toHaveTextContent("退出码 2");
    await userEvent.click(screen.getByText("pnpm build"));
    expect(onOpenOutput).toHaveBeenCalledWith(second);
  });

  it("does not manufacture a pass when no checks are configured or executed", () => {
    render(<VerificationView records={[]} detected={[]} running={false} hasTask onRun={vi.fn()} onRunAll={vi.fn()} onOpenOutput={vi.fn()} />);
    expect(screen.getByText("未从工程清单识别到验证命令")).toBeInTheDocument();
    expect(screen.getByText("尚未运行构建或测试")).toBeInTheDocument();
    expect(screen.queryByText("通过")).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "运行全部验证" })).toBeDisabled();
  });

  it("expires historical checks when source, configuration or unsaved buffers change", () => {
    const props = { records: [record()], detected: [], running: false, hasTask: true,
      onRun: vi.fn(), onRunAll: vi.fn(), onOpenOutput: vi.fn() };
    const view = render(<VerificationView {...props} currentRevision="source-one" currentConfigurationRevision="config-one" />);
    expect(screen.queryByText("已过期")).not.toBeInTheDocument();
    view.rerender(<VerificationView {...props} currentRevision="source-two" currentConfigurationRevision="config-one" />);
    expect(screen.getByText("已过期")).toBeInTheDocument();
    view.rerender(<VerificationView {...props} currentRevision="source-one" currentConfigurationRevision="config-two" />);
    expect(screen.getByText("已过期")).toBeInTheDocument();
    view.rerender(<VerificationView {...props} currentRevision="source-one" currentConfigurationRevision="config-one" dirty />);
    expect(screen.getByText("已过期")).toBeInTheDocument();
    expect(screen.getByText("pnpm test").closest("button")).toHaveTextContent("退出码 0");
    view.rerender(<VerificationView {...props} currentRevision="source-one" currentConfigurationRevision="config-one" dirty={false} />);
    expect(screen.queryByText("已过期")).not.toBeInTheDocument();
  });

  it("shows an active command as running and exposes cancellation without a failure verdict", async () => {
    const onCancel = vi.fn();
    render(<VerificationView records={[record({ status: "running", exitCode: null, finishedAt: "" })]} detected={[]} running hasTask onRun={vi.fn()} onRunAll={vi.fn()} onOpenOutput={vi.fn()} onCancel={onCancel} />);
    const result = screen.getByText("pnpm test").closest("button");
    expect(result).toHaveTextContent("运行中");
    expect(result).toHaveTextContent("退出码 无");
    expect(result).not.toHaveTextContent("失败");
    expect(result).not.toHaveTextContent("通过");
    await userEvent.click(screen.getByRole("button", { name: "停止验证" }));
    expect(onCancel).toHaveBeenCalledOnce();
  });

});
