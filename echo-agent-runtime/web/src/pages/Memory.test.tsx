import { describe, it, expect, vi, afterEach } from "vitest";
import { render, screen, fireEvent, waitFor, act, within } from "@testing-library/react";
import { Memory } from "./Memory";
import { ConfirmProvider } from "../components/ConfirmDialog";
import * as api from "../lib/api";

const ENTRY = {
  id: "m1",
  content: "用户偏好简体中文",
  type: "preference",
  tier: "semantic",
  importance: 0.75,
  created_at: "2026-07-27T09:00:00Z",
};

function mockApi() {
  return vi.spyOn(api, "apiFetch").mockImplementation(async (path: string) => {
    if (path === "/memory/search") {
      return {
        results: [{ entry: { ...ENTRY, id: "m2", tier: "semantic", content: "语义层命中" }, score: 0.9 }],
      } as never;
    }
    if (path.startsWith("/memory?")) return { entries: [ENTRY], total: 1 } as never;
    return {} as never;
  });
}

function renderMemory() {
  render(
    <ConfirmProvider>
      <Memory />
    </ConfirmProvider>,
  );
}

afterEach(() => {
  vi.restoreAllMocks();
});

describe("Memory 页", () => {
  it("explains that deleting a superseded version preserves the current fact", async () => {
    const spy = mockApi();
    spy.mockImplementation(async (path) => path.startsWith("/memory?")
      ? { entries: [{ ...ENTRY, superseded_by: "new-version" }], total: 1 } as never : {} as never);
    renderMemory();
    expect(await screen.findByText("已被新版本替代")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "删除该记忆条目" }));
    const dialog = await screen.findByRole("dialog");
    expect(within(dialog).getByText(/当前版本、其他会话摘要和后续学习不受影响/)).toBeInTheDocument();
    expect(within(dialog).queryByText(/旧历史将无法自动重新保存/)).not.toBeInTheDocument();
    fireEvent.click(within(dialog).getByRole("button", { name: "删除" }));
    await waitFor(() => expect(spy).toHaveBeenCalledWith("/memory/m1?override=true", { method: "DELETE" }));
  });
  it("a cleared search cannot be restored by its late response", async () => {
    let complete!: (value: unknown) => void;
    const spy = mockApi();
    renderMemory();
    fireEvent.change(await screen.findByLabelText(/语义搜索/), { target: { value: "偏好" } });
    fireEvent.click(screen.getByRole("button", { name: "执行搜索" }));
    await screen.findByText("语义层命中");
    const normal = spy.getMockImplementation()!;
    spy.mockImplementation((path, options) => path === "/memory/search"
      ? new Promise((resolve) => { complete = resolve; }) as never : normal(path, options));
    fireEvent.click(screen.getByRole("button", { name: "执行搜索" }));
    fireEvent.click(screen.getByRole("button", { name: /清除搜索/ }));
    await act(async () => complete({ results: [{ entry: ENTRY, score: 1 }] }));
    expect(screen.queryByText(/正在查看搜索结果/)).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "情景记忆" })).toBeEnabled();
  });

  it("only the latest submitted search may update the results", async () => {
    const pending: Array<(value: unknown) => void> = [];
    const spy = mockApi();
    const normal = spy.getMockImplementation()!;
    spy.mockImplementation((path, options) => path === "/memory/search"
      ? new Promise((resolve) => { pending.push(resolve); }) as never : normal(path, options));
    renderMemory();
    const input = await screen.findByLabelText(/语义搜索/);
    fireEvent.change(input, { target: { value: "old query" } });
    fireEvent.click(screen.getByRole("button", { name: "执行搜索" }));
    fireEvent.change(input, { target: { value: "new query" } });
    fireEvent.click(screen.getByRole("button", { name: "执行搜索" }));
    await act(async () => pending[1]({ results: [{ entry: { ...ENTRY, content: "new result" }, score: 1 }] }));
    await act(async () => pending[0]({ results: [{ entry: { ...ENTRY, content: "old result" }, score: 1 }] }));
    expect(screen.getByText("new result")).toBeInTheDocument();
    expect(screen.queryByText("old result")).not.toBeInTheDocument();
  });
  it("默认展示持久化语义记忆,重要度读 importance 字段", async () => {
    const spy = mockApi();
    renderMemory();

    await waitFor(() => expect(spy).toHaveBeenCalledWith("/memory?tier=semantic&limit=50&offset=0&include_all=true", expect.anything()));
    expect(await screen.findByText("用户偏好简体中文")).toBeInTheDocument();
    // 之前读的是后端不存在的 weight,这里恒显示 "-"。
    expect(screen.getByText(/重要度: 0.75/)).toBeInTheDocument();
  });

  it("切分层重新拉取对应 tier", async () => {
    const spy = mockApi();
    renderMemory();

    await screen.findByText("用户偏好简体中文");
    fireEvent.click(screen.getByRole("button", { name: "情景记忆" }));

    await waitFor(() => expect(spy).toHaveBeenCalledWith("/memory?tier=episodic&limit=50&offset=0&include_all=true", expect.anything()));
  });

  it("搜索带 all_scopes 全局检索,并解包 {entry,score}", async () => {
    const spy = mockApi();
    renderMemory();

    fireEvent.change(await screen.findByLabelText(/语义搜索/), {
      target: { value: "偏好" },
    });
    fireEvent.click(screen.getByRole("button", { name: "执行搜索" }));

    await waitFor(() =>
      expect(spy).toHaveBeenCalledWith("/memory/search", {
        method: "POST",
        body: JSON.stringify({ query: "偏好", limit: 20, all_scopes: true }),
      }),
    );
    expect(await screen.findByText("语义层命中")).toBeInTheDocument();
  });

  it("搜索态下禁用分层页签并显式提示当前是搜索结果", async () => {
    mockApi();
    renderMemory();

    fireEvent.change(await screen.findByLabelText(/语义搜索/), { target: { value: "偏好" } });
    fireEvent.keyDown(screen.getByLabelText(/语义搜索/), { key: "Enter" });

    expect(await screen.findByText(/正在查看搜索结果/)).toBeInTheDocument();
    // 分层与全局搜索是正交的两种视图,同时可点会让用户分不清在看什么。
    expect(screen.getByRole("button", { name: "情景记忆" })).toBeDisabled();
  });

  it("清除搜索回到分层浏览", async () => {
    const spy = mockApi();
    renderMemory();

    fireEvent.change(await screen.findByLabelText(/语义搜索/), { target: { value: "偏好" } });
    fireEvent.click(screen.getByRole("button", { name: "执行搜索" }));
    await screen.findByText("语义层命中");

    fireEvent.click(screen.getByRole("button", { name: /清除搜索/ }));

    expect(await screen.findByText("用户偏好简体中文")).toBeInTheDocument();
    expect(screen.getByLabelText(/语义搜索/)).toHaveValue("");
    expect(screen.getByRole("button", { name: "情景记忆" })).toBeEnabled();
    await waitFor(() => expect(spy.mock.calls.filter(([path]) => path.startsWith("/memory?")).length).toBeGreaterThan(1));
  });

  it("空关键词搜索直接退出搜索态,不发请求", async () => {
    const spy = mockApi();
    renderMemory();

    await screen.findByText("用户偏好简体中文");
    fireEvent.change(screen.getByLabelText(/语义搜索/), { target: { value: "   " } });
    fireEvent.click(screen.getByRole("button", { name: "执行搜索" }));

    await waitFor(() => expect(spy).not.toHaveBeenCalledWith("/memory/search", expect.anything()));
  });

  it("删除需二次确认,确认后发 DELETE", async () => {
    const spy = mockApi();
    renderMemory();

    fireEvent.click(await screen.findByRole("button", { name: "删除该记忆条目" }));
    expect(await screen.findByText(/旧历史将无法自动重新保存/)).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "删除" }));

    await waitFor(() => expect(spy).toHaveBeenCalledWith("/memory/m1?override=true", { method: "DELETE" }));
  });

  it("搜索结果里删除后本地剔除,不会退回分层列表", async () => {
    mockApi();
    renderMemory();

    fireEvent.change(await screen.findByLabelText(/语义搜索/), { target: { value: "偏好" } });
    fireEvent.click(screen.getByRole("button", { name: "执行搜索" }));
    await screen.findByText("语义层命中");

    fireEvent.click(screen.getByRole("button", { name: "删除该记忆条目" }));
    fireEvent.click(await screen.findByRole("button", { name: "删除" }));

    await waitFor(() => expect(screen.queryByText("语义层命中")).not.toBeInTheDocument());
    expect(screen.getByText(/正在查看搜索结果/)).toBeInTheDocument();
  });
});
