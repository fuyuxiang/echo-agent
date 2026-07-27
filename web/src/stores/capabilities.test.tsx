import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import { MemoryRouter } from "react-router";
import { Sidebar } from "../components/Sidebar";
import { useCapabilitiesStore } from "./capabilities";
import { useAuthStore } from "./auth";
import * as api from "../lib/api";
import { TOKEN_STORAGE_KEY } from "../lib/api";

beforeEach(() => {
  localStorage.clear();
  useCapabilitiesStore.getState().reset();
});

afterEach(() => {
  vi.restoreAllMocks();
});

describe("capabilities 探测", () => {
  it("并发调用只发一次请求", async () => {
    const spy = vi.spyOn(api, "apiFetch").mockResolvedValue({ admin: true });
    const { probe } = useCapabilitiesStore.getState();

    await Promise.all([probe(), probe(), probe()]);

    expect(spy).toHaveBeenCalledTimes(1);
    expect(useCapabilitiesStore.getState().admin).toBe(true);
  });

  it("已探测过不再重复请求", async () => {
    const spy = vi.spyOn(api, "apiFetch").mockResolvedValue({ admin: false });

    await useCapabilitiesStore.getState().probe();
    await useCapabilitiesStore.getState().probe();

    expect(spy).toHaveBeenCalledTimes(1);
    expect(useCapabilitiesStore.getState().admin).toBe(false);
  });

  it("探测失败按乐观策略取 true,老版本网关不至于把人锁在外面", async () => {
    vi.spyOn(api, "apiFetch").mockRejectedValue(new Error("404"));

    await useCapabilitiesStore.getState().probe();

    expect(useCapabilitiesStore.getState().admin).toBe(true);
  });
});

describe("auth 与 capabilities 联动", () => {
  it("换令牌登录后重置探测结果,避免沿用上一个令牌的权限", async () => {
    vi.spyOn(api, "apiFetch").mockResolvedValue({ admin: false });
    await useCapabilitiesStore.getState().probe();
    expect(useCapabilitiesStore.getState().admin).toBe(false);

    useAuthStore.getState().setToken("admin-token");

    expect(useCapabilitiesStore.getState().admin).toBeNull();
  });

  it("登出清掉令牌与探测结果", async () => {
    vi.spyOn(api, "apiFetch").mockResolvedValue({ admin: true });
    useAuthStore.getState().setToken("t");
    await useCapabilitiesStore.getState().probe();

    useAuthStore.getState().logout();

    expect(useAuthStore.getState().token).toBeNull();
    expect(localStorage.getItem(TOKEN_STORAGE_KEY)).toBeNull();
    expect(useCapabilitiesStore.getState().admin).toBeNull();
  });
});

describe("侧边栏登出入口", () => {
  it("点击后清空登录态 —— 此前唯一的退出方式是等 401", async () => {
    useAuthStore.getState().setToken("t");
    render(<MemoryRouter><Sidebar /></MemoryRouter>);

    fireEvent.click(screen.getByRole("button", { name: "退出登录" }));

    await waitFor(() => expect(useAuthStore.getState().token).toBeNull());
  });
});
