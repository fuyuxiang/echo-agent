import { act, renderHook } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { AutomationRunRecord } from "@/lib/types";
vi.mock("@/lib/agent-client", () => ({ automationRunResult: vi.fn() }));
import { automationRunResult } from "@/lib/agent-client";
import { useRunPreview } from "../automation/use-run-preview";
const run: AutomationRunRecord = { id: "r1", automationId: "a", automationName: "任务", status: "running", sessionId: "shared", startedAt: "2026-10-01T00:00:00Z", archived: false };
const value = (text: string) => ({ text, truncated: false });
beforeEach(() => { vi.useFakeTimers(); vi.mocked(automationRunResult).mockReset().mockResolvedValue(value("阶段结果")); });
afterEach(() => { vi.useRealTimers(); });

it("会话身份后来补齐时立即读取，同一运行中持续更新", async () => {
  const { result, rerender, unmount } = renderHook(({ item }) => useRunPreview(item), { initialProps: { item: { ...run, sessionId: undefined } as AutomationRunRecord } });
  expect(automationRunResult).not.toHaveBeenCalled();
  rerender({ item: run });
  await act(async () => {});
  expect(result.current.result?.text).toBe("阶段结果");
  vi.mocked(automationRunResult).mockResolvedValueOnce(value("新增进展"));
  await act(async () => { await vi.advanceTimersByTimeAsync(2_000); });
  expect(result.current.result?.text).toBe("新增进展");
  unmount();
});

it("慢请求不重叠，暂时读取失败保留内容且自动恢复", async () => {
  const { result, unmount } = renderHook(() => useRunPreview(run));
  await act(async () => {});
  let reject!: (error: Error) => void;
  vi.mocked(automationRunResult).mockImplementationOnce(() => new Promise((_, fail) => { reject = fail; }));
  await act(async () => { await vi.advanceTimersByTimeAsync(2_000); });
  act(() => { window.dispatchEvent(new Event("focus")); });
  await act(async () => { await vi.advanceTimersByTimeAsync(10_000); });
  expect(automationRunResult).toHaveBeenCalledTimes(2);
  expect(result.current.result?.text).toBe("阶段结果");
  await act(async () => { reject(new Error("暂时无法读取")); });
  expect(result.current.error).toBe("暂时无法读取");
  expect(result.current.result?.text).toBe("阶段结果");
  await act(async () => { await vi.advanceTimersByTimeAsync(2_000); });
  expect(result.current.error).toBe("");
  unmount();
});

it("沿用同一会话的历史运行独立读取，迟到响应不能覆盖新选择", async () => {
  let finish!: (preview: ReturnType<typeof value>) => void;
  vi.mocked(automationRunResult).mockImplementationOnce(() => new Promise(resolve => { finish = resolve; }));
  const { result, rerender, unmount } = renderHook(({ item }) => useRunPreview(item), { initialProps: { item: run } });
  rerender({ item: { ...run, id: "r2", status: "success", finishedAt: "2026-10-01T01:00:00Z" } });
  await act(async () => {});
  await act(async () => { finish(value("旧轮结果")); });
  expect(result.current.result?.text).toBe("阶段结果");
  expect(automationRunResult).toHaveBeenNthCalledWith(2, "r2");
  unmount();
});

it("结束时读取最终结果并短暂核对落盘，之后停止轮询", async () => {
  const { result, rerender, unmount } = renderHook(({ item }) => useRunPreview(item), { initialProps: { item: run } });
  await act(async () => {});
  vi.mocked(automationRunResult).mockResolvedValueOnce(value("阶段结果")).mockResolvedValue(value("唯一最终结论"));
  rerender({ item: { ...run, status: "success", finishedAt: new Date().toISOString() } });
  await act(async () => {});
  expect(result.current.result?.text).toBe("阶段结果");
  await act(async () => { await vi.advanceTimersByTimeAsync(1_500); });
  expect(result.current.result?.text).toBe("唯一最终结论");
  const calls = vi.mocked(automationRunResult).mock.calls.length;
  await act(async () => { await vi.advanceTimersByTimeAsync(30_000); });
  expect(automationRunResult).toHaveBeenCalledTimes(calls);
  unmount();
});

it("关闭详情后停止轮询，迟到结果不创建后续请求", async () => {
  let finish!: (preview: ReturnType<typeof value>) => void;
  vi.mocked(automationRunResult).mockImplementationOnce(() => new Promise(resolve => { finish = resolve; }));
  const { unmount } = renderHook(() => useRunPreview(run));
  unmount();
  await act(async () => { finish(value("迟到结果")); await vi.advanceTimersByTimeAsync(10_000); });
  expect(automationRunResult).toHaveBeenCalledTimes(1);
});
