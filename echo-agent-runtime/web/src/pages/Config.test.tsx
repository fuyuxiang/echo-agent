import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
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
    expect(spy).toHaveBeenCalledWith("/config", expect.anything());
  });

  it("刷新页面后从服务器恢复保存但未生效的状态", async () => {
    vi.spyOn(api, "apiFetch").mockImplementation(async (path: string) => {
      if (path === "/capabilities") return { admin: true } as never;
      return {
        memory: { enabled: false },
        _meta: {
          restart_required: true,
          pending_paths: ["memory.enabled"],
          running: { memory: { enabled: true } },
        },
      } as never;
    });
    const first = render(<Config />);
    await waitFor(() => expect(screen.getByRole("status")).toHaveTextContent("认知记忆"));
    expect(screen.getByRole("status")).toHaveTextContent("运行中配置不同");
    first.unmount();
    render(<Config />);
    await waitFor(() => expect(screen.getByRole("status")).toHaveTextContent("认知记忆"));
  });

  it("运行配置已与保存值一致时不提示重启", async () => {
    vi.spyOn(api, "apiFetch").mockImplementation(async (path: string) => {
      if (path === "/capabilities") return { admin: true } as never;
      return { _meta: { restart_required: false, pending_paths: [] } } as never;
    });
    render(<Config />);
    await waitFor(() => expect(screen.getByRole("button", { name: /保存/ })).toBeInTheDocument());
    expect(screen.queryByRole("status")).not.toBeInTheDocument();
  });

  it("锁定由启动设置管理的字段，保存其他字段时不提交锁定值", async () => {
    const spy = vi.spyOn(api, "apiFetch").mockImplementation(async (path: string) => {
      if (path === "/capabilities") return { admin: true } as never;
      return { memory: { enabled: true }, _meta: { managed_paths: ["memory.enabled"] } } as never;
    });
    render(<Config />);
    await waitFor(() => expect(screen.getByRole("switch", { name: "认知记忆" })).toBeDisabled());
    expect(screen.getByText(/部分设置由环境变量或启动参数管理/)).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: /保存/ }));
    await waitFor(() => expect(spy.mock.calls.some(([, options]) => options?.method === "PATCH")).toBe(true));
    const [, options] = spy.mock.calls.find(([, options]) => options?.method === "PATCH")!;
    const changes = JSON.parse(options!.body as string).changes;
    expect(changes).not.toHaveProperty("memory.enabled");
    expect(changes).toHaveProperty("ui.locale");
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
    expect(spy).toHaveBeenCalledWith("/config", expect.anything());
  });
});
