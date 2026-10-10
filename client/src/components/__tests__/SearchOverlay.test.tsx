import { beforeEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { SearchOverlay } from "../SearchOverlay";
import { useSessionsStore } from "@/stores/sessions-store";

vi.mock("@/lib/agent-client", () => ({
  sessionSearch: vi.fn(),
  agentListSessions: vi.fn(),
}));

const { sessionSearch, agentListSessions } = await import("@/lib/agent-client");

describe("SearchOverlay", () => {
  beforeEach(() => {
    vi.mocked(sessionSearch).mockReset().mockResolvedValue([]);
    vi.mocked(agentListSessions).mockReset().mockResolvedValue([]);
    useSessionsStore.setState({
      independent: [],
      workspaces: [],
      homeCwd: "/home",
      currentSessionId: null,
      filterStatus: null,
      filterDate: null,
      pendingSessionPatches: {},
    });
  });

  it("FTS 命中先 hydrate 完整摘要再打开", async () => {
    vi.mocked(sessionSearch).mockResolvedValue([{
      sessionId: "remote",
      title: "远程命中",
      cwd: "/workspace",
      snippet: "命中内容",
    }]);
    vi.mocked(agentListSessions).mockResolvedValue([{
      sessionId: "remote",
      title: "完整标题",
      cwd: "/workspace",
      currentModelId: "model-a",
    }]);
    const onSelect = vi.fn();
    const onClose = vi.fn();
    render(<SearchOverlay open onClose={onClose} onSelect={onSelect} />);

    fireEvent.change(screen.getByRole("textbox", { name: "搜索会话标题或内容" }), {
      target: { value: "远程" },
    });
    fireEvent.click(await screen.findByRole("button", { name: /\u8fdc\u7a0b\u547d\u4e2d/ }));

    await waitFor(() => expect(agentListSessions).toHaveBeenCalledWith("/workspace", true));
    expect(onSelect).toHaveBeenCalledWith("remote", "/workspace");
    expect(useSessionsStore.getState().independent[0]).toMatchObject({
      title: "完整标题",
      currentModelId: "model-a",
    });
    expect(onClose).toHaveBeenCalled();
  });

  it("FTS 失败显示错误与重试，不伪装成空结果", async () => {
    vi.mocked(sessionSearch).mockRejectedValue(new Error("index unavailable"));
    render(<SearchOverlay open onClose={vi.fn()} onSelect={vi.fn()} />);
    fireEvent.change(screen.getByRole("textbox", { name: "搜索会话标题或内容" }), {
      target: { value: "检索" },
    });
    expect(await screen.findByRole("alert")).toHaveTextContent("index unavailable");
    expect(screen.getByRole("button", { name: "重试搜索" })).toBeInTheDocument();
    expect(screen.queryByText("没有匹配的会话")).toBeNull();
  });

  it("本地与 FTS 重复时保留带 snippet 的 FTS 结果", async () => {
    useSessionsStore.getState().setIndependent([{
      sessionId: "same",
      title: "相同会话",
      cwd: "/home",
    }]);
    vi.mocked(sessionSearch).mockResolvedValue([{
      sessionId: "same",
      title: "相同会话",
      cwd: "/home",
      snippet: "独特正文片段",
    }]);
    render(<SearchOverlay open onClose={vi.fn()} onSelect={vi.fn()} />);
    fireEvent.change(screen.getByRole("textbox", { name: "搜索会话标题或内容" }), {
      target: { value: "相同" },
    });
    expect(await screen.findByText("独特正文片段")).toBeInTheDocument();
    expect(screen.getAllByRole("button", { name: /\u76f8\u540c\u4f1a\u8bdd/ })).toHaveLength(1);
  });

  it("普通搜索不显示已归档会话", async () => {
    useSessionsStore.getState().setIndependent([
      { sessionId: "active", title: "交付复盘", cwd: "/home", archived: false },
      { sessionId: "archived", title: "归档复盘", cwd: "/home", archived: true },
    ]);
    vi.mocked(sessionSearch).mockResolvedValue([
      { sessionId: "active", title: "交付复盘", cwd: "/home", snippet: "活动内容" },
      { sessionId: "archived", title: "归档复盘", cwd: "/home", snippet: "归档内容" },
    ]);

    render(<SearchOverlay open onClose={vi.fn()} onSelect={vi.fn()} />);
    fireEvent.change(screen.getByRole("textbox", { name: "搜索会话标题或内容" }), {
      target: { value: "复盘" },
    });

    expect(await screen.findByText("活动内容")).toBeInTheDocument();
    expect(screen.queryByText("归档复盘")).toBeNull();
    expect(screen.queryByText("归档内容")).toBeNull();
  });

  it("普通搜索不显示隐藏子代理会话", async () => {
    useSessionsStore.getState().setIndependent([
      { sessionId: "parent", title: "主任务", cwd: "/home" },
      { sessionId: "child", title: "子代理记录", cwd: "/home", hidden: true },
    ]);
    vi.mocked(sessionSearch).mockResolvedValue([
      { sessionId: "parent", title: "主任务", cwd: "/home", snippet: "主任务内容" },
      { sessionId: "child", title: "子代理记录", cwd: "/home", snippet: "子代理内容" },
    ]);

    render(<SearchOverlay open onClose={vi.fn()} onSelect={vi.fn()} />);
    fireEvent.change(screen.getByRole("textbox", { name: "搜索会话标题或内容" }), {
      target: { value: "任务" },
    });

    expect(await screen.findByText("主任务内容")).toBeInTheDocument();
    expect(screen.queryByText("子代理记录")).toBeNull();
    expect(screen.queryByText("子代理内容")).toBeNull();
  });

  it("迟到的隐藏子代理检索结果在打开前被拦截并给出正确入口", async () => {
    vi.mocked(sessionSearch).mockResolvedValue([{
      sessionId: "late-child",
      title: "迟到的子代理",
      cwd: "/workspace",
      snippet: "内部执行记录",
    }]);
    vi.mocked(agentListSessions).mockResolvedValue([{
      sessionId: "late-child",
      title: "迟到的子代理",
      cwd: "/workspace",
      hidden: true,
      sessionKind: "subagent",
    }]);
    const onSelect = vi.fn();
    const onClose = vi.fn();
    render(<SearchOverlay open onClose={onClose} onSelect={onSelect} />);
    fireEvent.change(screen.getByRole("textbox", { name: "搜索会话标题或内容" }), {
      target: { value: "子代理" },
    });
    fireEvent.click(await screen.findByRole("button", { name: /迟到的子代理/ }));

    expect(await screen.findByRole("alert")).toHaveTextContent("请从所属主任务的子代理面板打开");
    expect(onSelect).not.toHaveBeenCalled();
    expect(onClose).not.toHaveBeenCalled();
  });

  it("新查询的防抖窗口内不显示旧请求晚到的全文命中", async () => {
    vi.useFakeTimers();
    try {
      let finishOld!: (hits: Awaited<ReturnType<typeof sessionSearch>>) => void;
      vi.mocked(sessionSearch)
        .mockImplementationOnce(() => new Promise((resolve) => { finishOld = resolve; }))
        .mockResolvedValueOnce([{ sessionId: "new", title: "新查询结果", cwd: "/new" }]);
      render(<SearchOverlay open onClose={vi.fn()} onSelect={vi.fn()} />);
      const input = screen.getByRole("textbox", { name: "搜索会话标题或内容" });
      fireEvent.change(input, { target: { value: "旧查询" } });
      await act(async () => vi.advanceTimersByTime(250));
      fireEvent.change(input, { target: { value: "新查询" } });
      await act(async () => finishOld([{ sessionId: "old", title: "旧查询结果", cwd: "/old" }]));
      expect(screen.queryByText("旧查询结果")).not.toBeInTheDocument();
      expect(screen.getByRole("status")).toHaveTextContent("搜索中");
      await act(async () => vi.advanceTimersByTime(250));
      expect(screen.getByText("新查询结果")).toBeInTheDocument();
      fireEvent.change(input, { target: { value: "" } });
      expect(screen.queryByText("新查询结果")).not.toBeInTheDocument();
      expect(screen.queryByRole("status")).not.toBeInTheDocument();
    } finally {
      vi.useRealTimers();
    }
  });
});
