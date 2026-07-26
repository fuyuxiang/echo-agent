import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { apiFetch, getToken, TOKEN_STORAGE_KEY, RETURN_TO_STORAGE_KEY, setUnauthorizedHandler } from "./api";

const originalFetch = globalThis.fetch;

function jsonResponse(body: unknown, status = 200): Response {
  return {
    ok: status >= 200 && status < 300,
    status,
    statusText: status === 200 ? "OK" : "Error",
    json: async () => body,
  } as Response;
}

beforeEach(() => {
  localStorage.clear();
  sessionStorage.clear();
  setUnauthorizedHandler(() => {});
});

afterEach(() => {
  globalThis.fetch = originalFetch;
  vi.restoreAllMocks();
});

describe("apiFetch 鉴权头", () => {
  it("带上 localStorage 里的 token", async () => {
    localStorage.setItem(TOKEN_STORAGE_KEY, "secret");
    const spy = vi.fn().mockResolvedValue(jsonResponse({ ok: true }));
    globalThis.fetch = spy;

    await apiFetch("/stats");

    const headers = spy.mock.calls[0][1].headers as Record<string, string>;
    expect(headers.Authorization).toBe("Bearer secret");
    expect(spy.mock.calls[0][0]).toBe("/api/v1/stats");
  });

  it("getToken 在无 token 时返回空串而非 null", () => {
    expect(getToken()).toBe("");
  });
});

describe("apiFetch 401 处理", () => {
  it("调用 unauthorized 回调并抛错,不做整页跳转", async () => {
    globalThis.fetch = vi.fn().mockResolvedValue(jsonResponse({ error: "unauthorized" }, 401));
    const onUnauthorized = vi.fn();
    setUnauthorizedHandler(onUnauthorized);

    await expect(apiFetch("/tasks")).rejects.toThrow("Unauthorized");
    expect(onUnauthorized).toHaveBeenCalledTimes(1);
  });

  it("记下当前路径供登录后回跳", async () => {
    globalThis.fetch = vi.fn().mockResolvedValue(jsonResponse({}, 401));
    window.history.pushState({}, "", "/kanban?board=default");

    await expect(apiFetch("/tasks")).rejects.toThrow("Unauthorized");

    expect(sessionStorage.getItem(RETURN_TO_STORAGE_KEY)).toBe("/kanban?board=default");
  });

  it("已在登录页时不记回跳目标", async () => {
    globalThis.fetch = vi.fn().mockResolvedValue(jsonResponse({}, 401));
    window.history.pushState({}, "", "/login");

    await expect(apiFetch("/stats")).rejects.toThrow("Unauthorized");

    expect(sessionStorage.getItem(RETURN_TO_STORAGE_KEY)).toBeNull();
  });
});

describe("apiFetch 其他错误", () => {
  it("抛出后端 error 字段", async () => {
    globalThis.fetch = vi.fn().mockResolvedValue(jsonResponse({ error: "not found" }, 404));
    await expect(apiFetch("/tasks/x")).rejects.toThrow("not found");
  });

  it("响应体不是 JSON 时退回 statusText", async () => {
    globalThis.fetch = vi.fn().mockResolvedValue({
      ok: false,
      status: 500,
      statusText: "Internal Server Error",
      json: async () => { throw new Error("not json"); },
    } as unknown as Response);

    await expect(apiFetch("/tasks")).rejects.toThrow("Internal Server Error");
  });
});
