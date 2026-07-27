import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import { Config } from "./Config";
import * as api from "../lib/api";
import { useCapabilitiesStore } from "../stores/capabilities";

/**
 * The contract this page now depends on: /config is admin-guarded end to end,
 * so a non-admin session must never fire the request. Before the capabilities
 * probe existed the page always fetched and rendered "加载失败：403", which reads
 * like a server fault rather than a permission boundary.
 */
beforeEach(() => {
  useCapabilitiesStore.getState().reset();
});

afterEach(() => {
  vi.restoreAllMocks();
});

describe("Config 页", () => {
  it("admin 令牌下拉取并展示配置 JSON", async () => {
    const spy = vi.spyOn(api, "apiFetch").mockImplementation(async (path: string) => {
      if (path === "/capabilities") return { admin: true } as never;
      return { agent: { name: "echo" } } as never;
    });

    render(<Config />);

    await waitFor(() => expect(screen.getByText(/"echo"/)).toBeInTheDocument());
    expect(spy).toHaveBeenCalledWith("/config");
  });

  it("非 admin 令牌下不请求 /config,只提示权限不足", async () => {
    const spy = vi.spyOn(api, "apiFetch").mockImplementation(async (path: string) => {
      if (path === "/capabilities") return { admin: false } as never;
      throw new Error("403");
    });

    render(<Config />);

    await waitFor(() =>
      expect(screen.getByText(/需要管理员令牌/)).toBeInTheDocument(),
    );
    expect(spy).not.toHaveBeenCalledWith("/config");
    // 不能退化成通用的“加载失败”提示。
    expect(screen.queryByText(/加载失败/)).not.toBeInTheDocument();
  });

  it("探测失败时按乐观策略仍然请求,由端点自身的错误说话", async () => {
    const spy = vi.spyOn(api, "apiFetch").mockImplementation(async (path: string) => {
      if (path === "/capabilities") throw new Error("404");
      throw new Error("boom");
    });

    render(<Config />);

    await waitFor(() => expect(screen.getByText(/加载失败：boom/)).toBeInTheDocument());
    expect(spy).toHaveBeenCalledWith("/config");
  });
});
