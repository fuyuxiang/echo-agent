import { act, renderHook } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { ProviderListModel } from "../agent-client";
const request = vi.hoisted(() => vi.fn());
vi.mock("../agent-client", () => ({ providersTestModelConnection: request }));
import { useModelConnectionTests } from "../use-model-connection-tests";

const provider = { id: "p", providerKind: "custom" as const, apiKey: "••••" };
const model = { modelId: "p/model", providerId: "p", remoteModelId: "actual-model" };
const catalog: ProviderListModel = { providers: [provider], models: [model] };
afterEach(() => { vi.restoreAllMocks(); vi.useRealTimers(); request.mockReset(); });

describe("model connection diagnostics", () => {
  it("记录完整测试调用耗时，屏蔽前端密钥并防止同一模型重复测试", async () => {
    let now = 100;
    vi.spyOn(performance, "now").mockImplementation(() => now);
    let finish!: () => void;
    request.mockImplementation(() => new Promise<void>(resolve => { finish = resolve; }));
    const { result } = renderHook(() => useModelConnectionTests(catalog));
    let first!: Promise<void>;
    act(() => { first = result.current.test(provider, model); void result.current.test(provider, model); });
    expect(request).toHaveBeenCalledExactlyOnceWith({ ...provider, apiKey: undefined }, "actual-model");
    expect(result.current.results[model.modelId].status).toBe("testing");
    now = 1325;
    await act(async () => { finish(); await first; });
    expect(result.current.results[model.modelId]).toMatchObject({ status: "success", durationMs: 1225 });
  });

  it("配置重读后丢弃旧请求结果，新请求独立完成", async () => {
    let finishOld!: () => void;
    request.mockImplementationOnce(() => new Promise<void>(resolve => { finishOld = resolve; })).mockResolvedValueOnce(undefined);
    const { result, rerender } = renderHook(({ value }) => useModelConnectionTests(value), { initialProps: { value: catalog } });
    let first!: Promise<void>;
    act(() => { first = result.current.test(provider, model); });
    rerender({ value: { ...catalog } });
    expect(result.current.results).toEqual({});
    await act(async () => { await result.current.test(provider, model); });
    const current = result.current.results[model.modelId];
    await act(async () => { finishOld(); await first; });
    expect(result.current.results[model.modelId]).toBe(current);
  });

  it("测试结果十五分钟后清除，不作为永久可用状态", async () => {
    vi.useFakeTimers();
    request.mockResolvedValue(undefined);
    const { result } = renderHook(() => useModelConnectionTests(catalog));
    await act(async () => { await result.current.test(provider, model); });
    expect(result.current.results[model.modelId].status).toBe("success");
    act(() => vi.advanceTimersByTime(15 * 60_000));
    expect(result.current.results).toEqual({});
  });
});
