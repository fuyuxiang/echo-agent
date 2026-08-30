import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { renderHook, waitFor, act } from "@testing-library/react";
import { useApi } from "./use-api";
import * as api from "../lib/api";

beforeEach(() => {
  localStorage.clear();
});

afterEach(() => {
  vi.restoreAllMocks();
});

describe("useApi 三态", () => {
  it("首次渲染是 loading,data/error 为空", async () => {
    vi.spyOn(api, "apiFetch").mockImplementation(() => new Promise(() => {}));
    const { result } = renderHook(() => useApi<{ ok: boolean }>("/stats"));

    expect(result.current.loading).toBe(true);
    expect(result.current.data).toBeNull();
    expect(result.current.error).toBeNull();
  });

  it("成功后暴露 data 并结束 loading", async () => {
    vi.spyOn(api, "apiFetch").mockResolvedValue({ ok: true });
    const { result } = renderHook(() => useApi<{ ok: boolean }>("/stats"));

    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(result.current.data).toEqual({ ok: true });
    expect(result.current.error).toBeNull();
  });

  it("失败后暴露 error 而不是一直卡在 loading", async () => {
    vi.spyOn(api, "apiFetch").mockRejectedValue(new Error("boom"));
    const { result } = renderHook(() => useApi("/stats"));

    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(result.current.error).toBe("boom");
    expect(result.current.data).toBeNull();
  });

  it("非 Error 抛出物也能转成字符串错误", async () => {
    vi.spyOn(api, "apiFetch").mockRejectedValue("plain string");
    const { result } = renderHook(() => useApi("/stats"));

    await waitFor(() => expect(result.current.error).toBe("plain string"));
  });

  it("path 变化触发重新请求", async () => {
    const spy = vi.spyOn(api, "apiFetch").mockResolvedValue({ ok: true });
    const { result, rerender } = renderHook(({ path }) => useApi(path), {
      initialProps: { path: "/a" },
    });

    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(spy).toHaveBeenCalledWith("/a", expect.objectContaining({ signal: expect.anything() }));

    rerender({ path: "/b" });
    await waitFor(() => expect(spy).toHaveBeenCalledWith("/b", expect.objectContaining({ signal: expect.anything() })));
    expect(spy).toHaveBeenCalledTimes(2);
  });

  it("path 不变时重渲染不重复请求", async () => {
    const spy = vi.spyOn(api, "apiFetch").mockResolvedValue({ ok: true });
    const { result, rerender } = renderHook(() => useApi("/stats"));

    await waitFor(() => expect(result.current.loading).toBe(false));
    rerender();
    rerender();

    expect(spy).toHaveBeenCalledTimes(1);
  });

  it("refetch 清掉上次的 error", async () => {
    const spy = vi.spyOn(api, "apiFetch").mockRejectedValueOnce(new Error("boom"));
    const { result } = renderHook(() => useApi<{ ok: boolean }>("/stats"));

    await waitFor(() => expect(result.current.error).toBe("boom"));

    spy.mockResolvedValueOnce({ ok: true });
    await act(async () => { await result.current.refetch(); });

    expect(result.current.error).toBeNull();
    expect(result.current.data).toEqual({ ok: true });
  });
});
