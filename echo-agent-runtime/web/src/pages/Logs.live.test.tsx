import { act, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { Logs } from "./Logs";
import * as api from "../lib/api";

const ws = vi.hoisted(() => ({ onLog: () => {} }));
vi.mock("../hooks/use-ws", () => ({
  useWsSubscribe: (_channels: string[], handler: () => void) => { ws.onLog = handler; },
}));

function result(message: string) {
  return {
    logs: [{ ts: "2026-10-22T01:00:00Z", level: "INFO", message }],
    total: 640,
  };
}

function mockApi() {
  return vi.spyOn(api, "apiFetch").mockImplementation(async (path: string) => result(path) as never);
}

async function advance(ms: number) {
  await act(async () => { await vi.advanceTimersByTimeAsync(ms); });
}

async function emit(count = 1) {
  await act(async () => {
    for (let i = 0; i < count; i++) ws.onLog();
  });
}

beforeEach(() => { vi.useFakeTimers(); });
afterEach(() => { vi.restoreAllMocks(); vi.useRealTimers(); });

describe("Logs 实时刷新与交叉操作", () => {
  it("排队的实时刷新不能覆盖新的搜索结果", async () => {
    const spy = mockApi();
    await act(async () => { render(<Logs />); });
    await emit();
    await act(async () => {
      fireEvent.change(screen.getByLabelText("搜索..."), { target: { value: "error" } });
    });
    await advance(300);
    const query = "/logs?limit=200&offset=0&q=error";
    expect(screen.getByText(query)).toBeInTheDocument();
    await advance(700);
    expect(spy).toHaveBeenCalledTimes(2);
    expect(screen.getByText(query)).toBeInTheDocument();

    await emit();
    await advance(500);
    expect(spy).toHaveBeenCalledTimes(3);
    expect(spy).toHaveBeenLastCalledWith(query, expect.anything());
  });

  it.each([
    ["全部级别", "ERROR", "/logs?limit=200&offset=0&level=ERROR"],
    ["每页条数", "50", "/logs?limit=50&offset=0"],
  ])("修改 %s 后不再执行旧查询的刷新", async (label, value, query) => {
    const spy = mockApi();
    await act(async () => { render(<Logs />); });
    await emit();
    await act(async () => {
      fireEvent.change(screen.getByLabelText(label), { target: { value } });
    });
    await advance(1000);
    expect(spy).toHaveBeenCalledTimes(2);
    expect(screen.getByText(query)).toBeInTheDocument();
    await emit();
    await advance(500);
    expect(spy).toHaveBeenLastCalledWith(query, expect.anything());
    expect(spy).toHaveBeenCalledTimes(3);
  });

  it("浏览历史日志时取消排队刷新并提示暂停，返回首页后恢复", async () => {
    const spy = mockApi();
    await act(async () => { render(<Logs />); });
    await emit();
    await act(async () => { fireEvent.click(screen.getByRole("button", { name: /较旧/ })); });
    await emit(5);
    await advance(1000);
    expect(spy).toHaveBeenCalledTimes(2);
    expect(screen.getByText("/logs?limit=200&offset=200")).toBeInTheDocument();
    expect(screen.getByText("浏览历史日志时暂停实时刷新")).toBeInTheDocument();

    await act(async () => { fireEvent.click(screen.getByRole("button", { name: /较新/ })); });
    expect(screen.queryByText("浏览历史日志时暂停实时刷新")).not.toBeInTheDocument();
    await emit();
    await advance(500);
    expect(spy).toHaveBeenCalledTimes(4);
    expect(spy).toHaveBeenLastCalledWith("/logs?limit=200&offset=0", expect.anything());
  });

  it("关闭实时刷新立即取消排队任务，重新开启会主动追上最新日志", async () => {
    const spy = mockApi();
    await act(async () => { render(<Logs />); });
    await emit();
    await act(async () => { fireEvent.click(screen.getByRole("checkbox", { name: "实时" })); });
    await emit(5);
    await advance(1000);
    expect(spy).toHaveBeenCalledTimes(1);

    await act(async () => { fireEvent.click(screen.getByRole("checkbox", { name: "实时" })); });
    expect(spy).toHaveBeenCalledTimes(2);
    await advance(1000);
    expect(spy).toHaveBeenCalledTimes(2);
  });

  it("历史页重新开启实时开关仍停留在历史页", async () => {
    const spy = mockApi();
    await act(async () => { render(<Logs />); });
    await act(async () => { fireEvent.click(screen.getByRole("button", { name: /较旧/ })); });
    await act(async () => { fireEvent.click(screen.getByRole("checkbox", { name: "实时" })); });
    await act(async () => { fireEvent.click(screen.getByRole("checkbox", { name: "实时" })); });
    await emit();
    await advance(1000);
    expect(spy).toHaveBeenCalledTimes(2);
    expect(screen.getByText("/logs?limit=200&offset=200")).toBeInTheDocument();
    expect(screen.getByText("浏览历史日志时暂停实时刷新")).toBeInTheDocument();
  });

  it("持续日志按固定窗口合并刷新，不会被新事件无限推迟", async () => {
    const spy = mockApi();
    await act(async () => { render(<Logs />); });
    await emit(50);
    await advance(499);
    await emit(50);
    expect(spy).toHaveBeenCalledTimes(1);
    await advance(1);
    expect(spy).toHaveBeenCalledTimes(2);
    await advance(1000);
    expect(spy).toHaveBeenCalledTimes(2);
  });

  it("慢请求期间合并日志事件，完成后补刷一次而不反复取消请求", async () => {
    let resolve!: (value: never) => void;
    const spy = mockApi();
    spy.mockImplementationOnce(() => new Promise((done) => { resolve = done; }));
    await act(async () => { render(<Logs />); });
    const signal = spy.mock.calls[0][1]?.signal;
    await emit(50);
    await advance(1000);
    expect(spy).toHaveBeenCalledTimes(1);
    expect(signal?.aborted).toBe(false);
    await act(async () => { resolve(result("initial-response") as never); });
    await advance(500);
    expect(spy).toHaveBeenCalledTimes(2);
    expect(screen.queryByText("initial-response")).not.toBeInTheDocument();
  });

  it("手动刷新会合并已有的排队任务", async () => {
    const spy = mockApi();
    await act(async () => { render(<Logs />); });
    await emit();
    await act(async () => { fireEvent.click(screen.getByRole("button", { name: "刷新" })); });
    await advance(1000);
    expect(spy).toHaveBeenCalledTimes(2);
  });

  it("搜索变更会取消在途实时请求，迟到的响应不能覆盖新结果", async () => {
    const spy = mockApi();
    await act(async () => { render(<Logs />); });
    let resolve!: (value: never) => void;
    spy.mockImplementationOnce(() => new Promise((done) => { resolve = done; }));
    await emit();
    await advance(500);
    const signal = spy.mock.calls[1][1]?.signal;
    await act(async () => {
      fireEvent.change(screen.getByLabelText("搜索..."), { target: { value: "error" } });
    });
    await advance(300);
    expect(signal?.aborted).toBe(true);
    await act(async () => { resolve(result("stale-live-response") as never); });
    expect(screen.queryByText("stale-live-response")).not.toBeInTheDocument();
    expect(screen.getByText("/logs?limit=200&offset=0&q=error")).toBeInTheDocument();
    await advance(1000);
    expect(spy).toHaveBeenCalledTimes(3);
  });

  it("实时请求失败后停止空转，后续日志事件可以恢复刷新", async () => {
    const spy = mockApi();
    await act(async () => { render(<Logs />); });
    spy.mockRejectedValueOnce(new Error("offline"));
    await emit();
    await advance(500);
    expect(screen.getByText("加载失败：offline")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "刷新" })).toBeEnabled();
    await advance(2000);
    expect(spy).toHaveBeenCalledTimes(2);
    await emit();
    await advance(500);
    expect(spy).toHaveBeenCalledTimes(3);
    expect(screen.queryByText("加载失败：offline")).not.toBeInTheDocument();
    expect(screen.getByText("/logs?limit=200&offset=0")).toBeInTheDocument();
  });

  it("搜索尚未提交时暂停实时与手动刷新，清空输入后恢复", async () => {
    const spy = mockApi();
    await act(async () => { render(<Logs />); });
    await act(async () => {
      fireEvent.change(screen.getByLabelText("搜索..."), { target: { value: "error" } });
    });
    expect(screen.getByRole("button", { name: "刷新" })).toBeDisabled();
    await emit(10);
    await advance(200);
    await act(async () => {
      fireEvent.change(screen.getByLabelText("搜索..."), { target: { value: "" } });
    });
    expect(screen.getByRole("button", { name: "刷新" })).toBeEnabled();
    await advance(1000);
    expect(spy).toHaveBeenCalledTimes(1);
    await emit();
    await advance(500);
    expect(spy).toHaveBeenCalledTimes(2);
  });

  it("请求中禁用分页和刷新并暴露忙碌状态", async () => {
    const spy = mockApi();
    await act(async () => { render(<Logs />); });
    let resolve!: (value: never) => void;
    spy.mockImplementationOnce(() => new Promise((done) => { resolve = done; }));
    await act(async () => { fireEvent.click(screen.getByRole("button", { name: /较旧/ })); });
    expect(screen.getByRole("log")).toHaveAttribute("aria-busy", "true");
    expect(screen.getByRole("button", { name: /较旧/ })).toBeDisabled();
    expect(screen.getByRole("button", { name: /较新/ })).toBeDisabled();
    expect(screen.getByRole("button", { name: "刷新" })).toBeDisabled();
    await act(async () => { resolve(result("older-page") as never); });
    expect(screen.getByRole("log")).toHaveAttribute("aria-busy", "false");
    expect(screen.getByText("older-page")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /较新/ })).toBeEnabled();
  });

  it("卸载后取消排队任务，重新挂载不会残留刷新", async () => {
    const spy = mockApi();
    let unmount!: () => void;
    await act(async () => { ({ unmount } = render(<Logs />)); });
    await emit();
    await act(async () => { unmount(); });
    await advance(1000);
    expect(spy).toHaveBeenCalledTimes(1);
    await act(async () => { render(<Logs />); });
    await advance(1000);
    expect(spy).toHaveBeenCalledTimes(2);
    await emit();
    await advance(500);
    expect(spy).toHaveBeenCalledTimes(3);
  });
});
