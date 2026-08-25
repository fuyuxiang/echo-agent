import { describe, it, expect, vi, afterEach } from "vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import { Sessions } from "./Sessions";
import { Channels } from "./Channels";
import * as api from "../lib/api";

afterEach(() => {
  vi.restoreAllMocks();
});

/**
 * 这两页的数据都会在界面之外发生变化(其他渠道持续聊天、通道掉线重连),而后端的
 * sessions / channels WS 频道都没有接线。手动刷新入口是它们唯一的更新途径,所以
 * 按钮存在与否本身就是功能点。
 */
describe("Sessions 页", () => {
  const SESSIONS = {
    sessions: [
      { key: "feishu:oc 1", message_count: 3, updated_at: "2026-07-27T09:00:00Z" },
      { key: "cli:local", message_count: 1, updated_at: "2026-07-27T08:00:00Z" },
    ],
  };

  function mockApi() {
    return vi.spyOn(api, "apiFetch").mockImplementation(async (path: string) => {
      if (path === "/sessions") return SESSIONS as never;
      return { messages: [{ role: "user", content: "你好" }] } as never;
    });
  }

  it("点击会话按编码后的 key 拉取历史", async () => {
    const spy = mockApi();
    render(<Sessions />);

    fireEvent.click(await screen.findByText("feishu:oc 1"));

    await waitFor(() =>
      expect(spy).toHaveBeenCalledWith("/sessions/feishu%3Aoc%201/history"),
    );
    expect(await screen.findByText("你好")).toBeInTheDocument();
  });

  it("刷新按钮同时重取列表和当前会话历史", async () => {
    const spy = mockApi();
    render(<Sessions />);

    fireEvent.click(await screen.findByText("cli:local"));
    await waitFor(() => expect(spy).toHaveBeenCalledWith("/sessions/cli%3Alocal/history"));

    const before = spy.mock.calls.length;
    fireEvent.click(screen.getByRole("button", { name: "刷新" }));

    await waitFor(() => {
      const added = spy.mock.calls.slice(before).map((c) => c[0]);
      expect(added).toContain("/sessions");
      expect(added).toContain("/sessions/cli%3Alocal/history");
    });
  });

  it("未选会话时刷新只重取列表", async () => {
    const spy = mockApi();
    render(<Sessions />);

    await screen.findByText("cli:local");
    const before = spy.mock.calls.length;
    fireEvent.click(screen.getByRole("button", { name: "刷新" }));

    await waitFor(() => expect(spy.mock.calls.length).toBe(before + 1));
    expect(spy.mock.calls[before][0]).toBe("/sessions");
  });

  it("历史加载失败时显示错误,不静默留空", async () => {
    vi.spyOn(api, "apiFetch").mockImplementation(async (path: string) => {
      if (path === "/sessions") return SESSIONS as never;
      throw new Error("boom");
    });
    render(<Sessions />);

    fireEvent.click(await screen.findByText("cli:local"));

    expect(await screen.findByText(/加载历史失败：boom/)).toBeInTheDocument();
  });

  it("搜索按 key 过滤", async () => {
    mockApi();
    render(<Sessions />);

    await screen.findByText("cli:local");
    fireEvent.change(screen.getByLabelText("搜索会话..."), { target: { value: "feishu" } });

    expect(screen.getByText("feishu:oc 1")).toBeInTheDocument();
    expect(screen.queryByText("cli:local")).not.toBeInTheDocument();
  });

  /**
   * 后端展示视图给工具调用 / 工具结果打了 internal 标记(session/manager.py:
   * display_messages)。旧渲染逻辑只分「user 靠右、其余靠左」,于是工具输出被
   * 显示成 Agent 说的话 —— 用户读到的是一段自己从未收到过的回复。
   */
  function mockHistoryWithTools() {
    return vi.spyOn(api, "apiFetch").mockImplementation(async (path: string) => {
      if (path === "/sessions") return SESSIONS as never;
      return {
        messages: [
          { role: "user", content: "北京天气" },
          { role: "assistant", content: "", internal: true, name: "web_search" },
          { role: "tool", content: "晴 28C", internal: true, name: "web_search" },
          { role: "assistant", content: "北京今天晴。" },
        ],
        total: 4,
        returned: 4,
      } as never;
    });
  }

  it("工具调用与结果折叠显示,不冒充 Agent 气泡", async () => {
    mockHistoryWithTools();
    render(<Sessions />);

    fireEvent.click(await screen.findByText("cli:local"));

    // 真正的对话内容照常显示。
    expect(await screen.findByText("北京今天晴。")).toBeInTheDocument();
    expect(screen.getByText("北京天气")).toBeInTheDocument();
    // 工具条目带工具名的摘要行,而不是一条普通气泡。
    expect(screen.getByText("调用工具：web_search")).toBeInTheDocument();
    expect(screen.getByText("工具结果：web_search")).toBeInTheDocument();
  });

  it("工具内容默认收起,展开后可读", async () => {
    mockHistoryWithTools();
    render(<Sessions />);

    fireEvent.click(await screen.findByText("cli:local"));
    const summary = await screen.findByText("工具结果：web_search");

    const details = summary.closest("details") as HTMLDetailsElement;
    expect(details.open).toBe(false);

    // 内容在 DOM 里(排查问题时需要),只是默认不展开。
    expect(screen.getByText("晴 28C")).toBeInTheDocument();
  });

  it("真实 Agent 回复仍渲染为普通气泡", async () => {
    mockHistoryWithTools();
    render(<Sessions />);

    fireEvent.click(await screen.findByText("cli:local"));
    const reply = await screen.findByText("北京今天晴。");

    expect(reply.closest("details")).toBeNull();
  });
});

describe("Channels 页", () => {
  const CHANNELS = {
    channels: [
      { name: "feishu", enabled: true, running: true },
      { name: "wecom", enabled: true, running: false },
    ],
  };

  it("副标题读 enabled 而不是后端不存在的 type 字段", async () => {
    vi.spyOn(api, "apiFetch").mockResolvedValue(CHANNELS);
    render(<Channels />);

    await screen.findByText("feishu");
    expect(screen.getAllByText("已启用")).toHaveLength(2);
    expect(screen.getByText("在线")).toBeInTheDocument();
    expect(screen.getByText("未连接")).toBeInTheDocument();
  });

  it("刷新按钮重新拉取通道状态", async () => {
    const spy = vi.spyOn(api, "apiFetch").mockResolvedValue(CHANNELS);
    render(<Channels />);

    await screen.findByText("feishu");
    fireEvent.click(screen.getByRole("button", { name: /刷新/ }));

    await waitFor(() => expect(spy.mock.calls.filter((c) => c[0] === "/channels").length).toBe(2));
  });

  it("加载失败时显示错误", async () => {
    vi.spyOn(api, "apiFetch").mockRejectedValue(new Error("boom"));
    render(<Channels />);

    expect(await screen.findByText(/加载失败：boom/)).toBeInTheDocument();
  });
});
