import { describe, it, expect, vi, afterEach } from "vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import { Logs } from "./Logs";
import * as api from "../lib/api";

/** 造 n 条日志,内容里带序号方便断言取到的是哪一页。 */
function page(n: number, tag: string) {
  return {
    logs: Array.from({ length: n }, (_, i) => ({
      ts: "2026-07-27T09:00:00Z",
      level: "INFO",
      message: `${tag}-${i}`,
    })),
    total: 640,
  };
}

function mockApi() {
  return vi.spyOn(api, "apiFetch").mockImplementation(async (path: string) => {
    const offset = Number(new URLSearchParams(path.split("?")[1]).get("offset") ?? 0);
    return page(200, `off${offset}`) as never;
  });
}

afterEach(() => {
  vi.restoreAllMocks();
});

describe("Logs 页分页", () => {
  it("首屏按默认页大小从 offset=0 拉取", async () => {
    const spy = mockApi();
    render(<Logs />);

    await waitFor(() => expect(spy).toHaveBeenCalledWith("/logs?limit=200&offset=0", expect.anything()));
    expect(await screen.findByText("off0-0")).toBeInTheDocument();
  });

  it("显示区间与总数,首页禁用“较新”", async () => {
    mockApi();
    render(<Logs />);

    expect(await screen.findByText("第 1-200 条 / 共 640 条")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /较新/ })).toBeDisabled();
    expect(screen.getByRole("button", { name: /较旧/ })).toBeEnabled();
  });

  it("翻页推进 offset,能到达缓冲区里更早的日志", async () => {
    const spy = mockApi();
    render(<Logs />);

    fireEvent.click(await screen.findByRole("button", { name: /较旧/ }));

    await waitFor(() => expect(spy).toHaveBeenCalledWith("/logs?limit=200&offset=200", expect.anything()));
    expect(await screen.findByText("第 201-400 条 / 共 640 条")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /较新/ })).toBeEnabled();
  });

  it("改筛选条件时回到第一页,避免 offset 越过新结果集导致空白", async () => {
    const spy = mockApi();
    render(<Logs />);

    fireEvent.click(await screen.findByRole("button", { name: /较旧/ }));
    await waitFor(() => expect(spy).toHaveBeenCalledWith("/logs?limit=200&offset=200", expect.anything()));

    fireEvent.change(screen.getByLabelText("全部级别"), { target: { value: "ERROR" } });

    await waitFor(() =>
      expect(spy).toHaveBeenCalledWith("/logs?limit=200&offset=0&level=ERROR", expect.anything()),
    );
  });

  it("改每页条数同样回到第一页", async () => {
    const spy = mockApi();
    render(<Logs />);

    fireEvent.click(await screen.findByRole("button", { name: /较旧/ }));
    await waitFor(() => expect(spy).toHaveBeenCalledWith("/logs?limit=200&offset=200", expect.anything()));

    fireEvent.change(screen.getByLabelText("每页条数"), { target: { value: "50" } });

    await waitFor(() => expect(spy).toHaveBeenCalledWith("/logs?limit=50&offset=0", expect.anything()));
  });

  it("搜索词做 URL 编码", async () => {
    const spy = mockApi();
    render(<Logs />);

    fireEvent.change(await screen.findByLabelText("搜索..."), {
      target: { value: "a b&c" },
    });

    await waitFor(() =>
      expect(spy).toHaveBeenCalledWith("/logs?limit=200&offset=0&q=a%20b%26c", expect.anything()),
    );
  });

  it("最后一页禁用“较旧”", async () => {
    vi.spyOn(api, "apiFetch").mockImplementation(async () => {
      return { logs: page(40, "tail").logs, total: 40 } as never;
    });
    render(<Logs />);

    await waitFor(() =>
      expect(screen.getByRole("button", { name: /较旧/ })).toBeDisabled(),
    );
  });

  it("请求失败时显示错误而不是空列表,且不渲染分页条", async () => {
    vi.spyOn(api, "apiFetch").mockRejectedValue(new Error("boom"));
    render(<Logs />);

    expect(await screen.findByText(/加载失败：boom/)).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /较旧/ })).not.toBeInTheDocument();
  });
});
