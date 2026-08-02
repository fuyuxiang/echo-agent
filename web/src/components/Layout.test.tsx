import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import { MemoryRouter, Routes, Route } from "react-router";
import { Layout } from "./Layout";
import { useCapabilitiesStore } from "../stores/capabilities";
import { useAuthStore } from "../stores/auth";
import * as api from "../lib/api";

const originalFetch = globalThis.fetch;

function jsonResponse(body: unknown, status = 200): Response {
  return {
    ok: status >= 200 && status < 300,
    status,
    statusText: status === 200 ? "OK" : "Error",
    json: async () => body,
  } as Response;
}

/**
 * Layout 的登录闸门。
 *
 * 后端在无 token 部署下明确允许开放访问（auth.authenticate_token 无 token 时
 * 恒真），但前端曾把 `!!token` 当作登录状态：空 token 被 Layout 踢到 /login，
 * Login 用空 token 探测 /stats 成功后跳回 /，Layout 再踢出去 —— 只有随便填一个
 * 非空假 token 才能绕过。这里钉住两侧行为，避免任何一边单独回退。
 */

function renderAt(path = "/") {
  return render(
    <MemoryRouter initialEntries={[path]}>
      <Routes>
        <Route element={<Layout />}>
          <Route index element={<div>dashboard-content</div>} />
        </Route>
        <Route path="/login" element={<div>login-page</div>} />
      </Routes>
    </MemoryRouter>,
  );
}

beforeEach(() => {
  localStorage.clear();
  useCapabilitiesStore.getState().reset();
  useAuthStore.setState({ token: null });
});

afterEach(() => {
  globalThis.fetch = originalFetch;
  vi.restoreAllMocks();
});

describe("Layout 登录闸门", () => {
  it("open 模式（authRequired=false）空 token 也能进入 dashboard", async () => {
    vi.spyOn(api, "apiFetch").mockResolvedValue({ admin: true, authRequired: false });

    renderAt();

    expect(await screen.findByText("dashboard-content")).toBeTruthy();
    expect(screen.queryByText("login-page")).toBeNull();
  });

  it("需要认证且无 token 时跳转 /login", async () => {
    vi.spyOn(api, "apiFetch").mockResolvedValue({ admin: true, authRequired: true });

    renderAt();

    expect(await screen.findByText("login-page")).toBeTruthy();
  });

  it("有 token 时直接进入，不必等探测返回", () => {
    // 探测永不 resolve：有 token 的常规部署不应被探测延迟阻塞首屏。
    vi.spyOn(api, "apiFetch").mockReturnValue(new Promise(() => {}) as never);
    useAuthStore.setState({ token: "tok" });

    renderAt();

    expect(screen.getByText("dashboard-content")).toBeTruthy();
  });

  it("探测未返回前不抢先跳转 /login", () => {
    vi.spyOn(api, "apiFetch").mockReturnValue(new Promise(() => {}) as never);

    renderAt();

    // 猜"需要认证"会重演回环；猜"开放"会让真部署闪现 dashboard。两者都不做。
    expect(screen.queryByText("login-page")).toBeNull();
    expect(screen.queryByText("dashboard-content")).toBeNull();
  });

  it("老网关不返回 authRequired 时按需要认证处理", async () => {
    // 静默即视为开放会让真正需要 token 的部署把登录页藏掉。
    vi.spyOn(api, "apiFetch").mockResolvedValue({ admin: true });

    renderAt();

    expect(await screen.findByText("login-page")).toBeTruthy();
  });

  it("探测失败（如 401）按需要认证处理", async () => {
    vi.spyOn(api, "apiFetch").mockRejectedValue(new Error("Unauthorized"));

    renderAt();

    expect(await screen.findByText("login-page")).toBeTruthy();
    await waitFor(() =>
      expect(useCapabilitiesStore.getState().authRequired).toBe(true),
    );
  });

  it("真实 apiFetch 收到 401 后进入登录页,不停在空白屏", async () => {
    // Do not mock apiFetch here: its 401 handler logs out and resets the
    // capability generation. That side effect is the regression this test
    // must exercise; mocking apiFetch hid it and let the blank screen ship.
    globalThis.fetch = vi.fn().mockResolvedValue(
      jsonResponse({ error: "unauthorized" }, 401),
    );

    renderAt();

    expect(await screen.findByText("login-page")).toBeTruthy();
    expect(useAuthStore.getState().token).toBeNull();
    expect(useCapabilitiesStore.getState().authRequired).toBe(true);
  });
});
