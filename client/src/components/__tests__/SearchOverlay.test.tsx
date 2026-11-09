import { beforeEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { SearchOverlay } from "../SearchOverlay";
import { useSessionsStore } from "@/stores/sessions-store";
import { useGroupStore } from "@/features/groups/group-store";
import type { SearchHit, SessionSearchPage } from "@/lib/types";
import type { Group } from "@/features/groups/group-client";
import { TrayActionCancelled } from "@/lib/desktop-tray-connection";
import userEvent from "@testing-library/user-event";

vi.mock("@/lib/agent-client", () => ({
  sessionSearchPage: vi.fn(),
  agentListSessions: vi.fn(),
}));

const { sessionSearchPage, agentListSessions } = await import("@/lib/agent-client");

const page = (results: SearchHit[] = [], nextOffset: number | null = null, bootstrapping = false): SessionSearchPage => ({
  results, nextOffset, bootstrapping,
});

describe("SearchOverlay", () => {
  beforeEach(() => {
    useGroupStore.setState({ groups: [] });
    vi.mocked(sessionSearchPage).mockReset().mockResolvedValue(page([]));
    vi.mocked(agentListSessions).mockReset().mockResolvedValue([]);
    useSessionsStore.setState({ deletedSessionIds: [],
      independent: [],
      workspaces: [],
      homeCwd: "/home",
      currentSessionId: null,
      filterStatus: null,
      filterDate: null,
      pendingSessionPatches: {},
    });
  });

  it("全部任务同时显示专家团入口，打开失败保留列表并允许重试", async () => {
    useGroupStore.setState({ groups: [
      { id: "group-one", name: "产品评审", archived: false, updatedAt: 1 } as Group,
      { id: "archived", name: "归档评审", archived: true, updatedAt: 2 } as Group,
    ] });
    const onSelectGroup = vi.fn().mockRejectedValueOnce(new Error("暂时不可用")).mockResolvedValue(undefined);
    const onClose = vi.fn();
    render(<SearchOverlay open onClose={onClose} onSelect={vi.fn()} onSelectGroup={onSelectGroup} />);
    expect(screen.queryByText("归档评审")).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "产品评审 默认工作区" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("暂时不可用");
    expect(onClose).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "重试打开" }));
    await waitFor(() => expect(onClose).toHaveBeenCalledOnce());
    expect(onSelectGroup).toHaveBeenCalledWith("group-one");
  });

  it("FTS 命中先 hydrate 完整摘要再打开", async () => {
    vi.mocked(sessionSearchPage).mockResolvedValue(page([{
      sessionId: "remote",
      title: "远程命中",
      cwd: "/workspace",
      snippet: "命中内容",
    }]));
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

  it("取消专家团跳转后保留搜索并允许重新选择，不显示失败提示", async () => {
    useGroupStore.setState({ groups: [{ id: "group", name: "产品评审", archived: false, updatedAt: 1 } as Group] });
    const onClose = vi.fn();
    const onSelectGroup = vi.fn().mockRejectedValueOnce(new TrayActionCancelled()).mockResolvedValue(undefined);
    render(<SearchOverlay open onClose={onClose} onSelect={vi.fn()} onSelectGroup={onSelectGroup} />);
    fireEvent.click(screen.getByRole("button", { name: "产品评审 默认工作区" }));
    await waitFor(() => expect(screen.getByRole("button", { name: "产品评审 默认工作区" })).toBeEnabled());
    expect(screen.queryByRole("alert")).toBeNull();
    expect(onClose).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "产品评审 默认工作区" }));
    await waitFor(() => expect(onClose).toHaveBeenCalledOnce());
  });

  it("超过 30 条本地任务可继续显示，最近任务优先且同名目录可辨识", async () => {
    useSessionsStore.getState().setIndependent(Array.from({length:65}, (_, index) => ({
      sessionId:`history-${index}`, title:"交付复盘", cwd:`/projects/${index}`,
      updatedAt:new Date(2026,10,6,0,index).toISOString(),
    })));
    render(<SearchOverlay open onClose={vi.fn()} onSelect={vi.fn()} />);
    const list=screen.getByRole("list");
    expect(list.children).toHaveLength(30);
    expect(list.querySelector("button")).toHaveAccessibleName("交付复盘 /projects/64");
    expect(screen.getByText(/显示更多本地会话/)).toHaveRole("button");
    fireEvent.click(screen.getByText(/显示更多本地会话/));
    expect(list.children).toHaveLength(60);
    fireEvent.click(screen.getByText(/显示更多本地会话/));
    expect(list.children).toHaveLength(65);
    expect(list.lastElementChild?.querySelector("button")).toHaveAccessibleName("交付复盘 /projects/0");
    expect(screen.queryByText(/显示更多本地会话/)).toBeNull();
  });

  it("输入框通过方向键选择同名任务，Enter 打开准确目录，IME Enter 不打开", async () => {
    const user = userEvent.setup();
    useSessionsStore.getState().setIndependent([
      {sessionId:"one",title:"同名任务",cwd:"/one"},
      {sessionId:"two",title:"同名任务",cwd:"/two"},
    ]);
    const onSelect=vi.fn();
    render(<SearchOverlay open onClose={vi.fn()} onSelect={onSelect} />);
    const input=screen.getByRole("textbox",{name:"搜索会话标题或内容"});
    input.focus();
    fireEvent.keyDown(input,{key:"Enter",keyCode:229});
    expect(onSelect).not.toHaveBeenCalled();
    await user.keyboard("{ArrowDown}{ArrowDown}");
    expect(screen.getByRole("button",{name:"同名任务 /two"})).toHaveFocus();
    await user.keyboard("{Enter}");
    expect(onSelect).toHaveBeenCalledWith("two","/two");
  });

  it("全文返回替换本地重复记录时保留所选任务的键盘焦点", async () => {
    const user=userEvent.setup();
    let finish!: (result: SessionSearchPage) => void;
    vi.mocked(sessionSearchPage).mockImplementation(()=>new Promise(resolve=>{finish=resolve;}));
    useSessionsStore.getState().setIndependent([{sessionId:"focused",title:"交付复盘",cwd:"/workspace"}]);
    const onSelect=vi.fn();
    render(<SearchOverlay open onClose={vi.fn()} onSelect={onSelect} />);
    await user.type(screen.getByRole("textbox",{name:"搜索会话标题或内容"}),"交付");
    await user.keyboard("{ArrowDown}");
    expect(screen.getByRole("button",{name:"交付复盘 /workspace"})).toHaveFocus();
    await waitFor(()=>expect(sessionSearchPage).toHaveBeenCalledOnce());
    await act(async()=>finish(page([{sessionId:"focused",title:"交付复盘",cwd:"/workspace",snippet:"正文命中内容"}])));
    const result=screen.getByText("正文命中内容").closest("button");
    expect(result).toHaveFocus();
    await user.keyboard("{Enter}");
    expect(onSelect).toHaveBeenCalledWith("focused","/workspace");
  });

  it("FTS 失败显示错误与重试，不伪装成空结果", async () => {
    vi.mocked(sessionSearchPage).mockRejectedValue(new Error("index unavailable"));
    render(<SearchOverlay open onClose={vi.fn()} onSelect={vi.fn()} />);
    fireEvent.change(screen.getByRole("textbox", { name: "搜索会话标题或内容" }), {
      target: { value: "检索" },
    });
    expect(await screen.findByRole("alert")).toHaveTextContent("index unavailable");
    expect(screen.getByRole("button", { name: "重试搜索" })).toBeInTheDocument();
    expect(screen.queryByText("没有匹配的会话")).toBeNull();
  });

  it.each(["session", "group"])("快速 Enter 打开 %s 失败后，防抖搜索不能清除重试入口", async (kind) => {
    vi.useFakeTimers();
    try {
      const select = vi.fn().mockRejectedValueOnce(new Error("读取失败")).mockResolvedValue(undefined);
      const onClose = vi.fn();
      if (kind === "session") {
        useSessionsStore.getState().setIndependent([{ sessionId: "local", title: "交付复盘", cwd: "/review" }]);
      } else {
        useGroupStore.setState({ groups: [{ id: "group", name: "交付复盘", archived: false, updatedAt: 1 } as Group] });
      }
      render(<SearchOverlay open onClose={onClose} onSelect={select} onSelectGroup={select} />);
      const input = screen.getByRole("textbox", { name: "搜索会话标题或内容" });
      fireEvent.change(input, { target: { value: "交付" } });
      await act(async () => { fireEvent.keyDown(input, { key: "Enter" }); });
      expect(screen.getByRole("alert")).toHaveTextContent("打开失败：读取失败");
      await act(async () => { await vi.advanceTimersByTimeAsync(250); });
      expect(sessionSearchPage).toHaveBeenCalledOnce();
      expect(screen.getByRole("button", { name: "重试打开" })).toBeEnabled();
      expect(screen.getByRole("alert")).toHaveTextContent("打开失败：读取失败");
      expect(onClose).not.toHaveBeenCalled();
      await act(async () => { fireEvent.click(screen.getByRole("button", { name: "重试打开" })); });
      expect(select).toHaveBeenCalledTimes(2);
      expect(onClose).toHaveBeenCalledOnce();
    } finally { vi.useRealTimers(); }
  });

  it("搜索与打开同时失败时，各自保留原因并重试原操作", async () => {
    vi.useFakeTimers();
    try {
      let rejectSearch!: (error: Error) => void;
      vi.mocked(sessionSearchPage).mockImplementationOnce(() => new Promise((_, reject) => { rejectSearch = reject; }));
      useSessionsStore.getState().setIndependent([{ sessionId: "local", title: "交付复盘", cwd: "/review" }]);
      const onSelect = vi.fn().mockRejectedValueOnce(new Error("会话不可读")).mockResolvedValue(undefined);
      const onClose = vi.fn();
      render(<SearchOverlay open onClose={onClose} onSelect={onSelect} />);
      const input = screen.getByRole("textbox", { name: "搜索会话标题或内容" });
      fireEvent.change(input, { target: { value: "交付" } });
      await act(async () => { await vi.advanceTimersByTimeAsync(250); });
      await act(async () => { fireEvent.keyDown(input, { key: "Enter" }); });
      await act(async () => { rejectSearch(new Error("索引不可用")); });
      expect(screen.getAllByRole("alert")).toHaveLength(2);
      expect(screen.getByText("打开失败：会话不可读")).toBeInTheDocument();
      expect(screen.getByText("搜索失败：索引不可用")).toBeInTheDocument();
      await act(async () => { fireEvent.click(screen.getByRole("button", { name: "重试搜索" })); });
      expect(screen.queryByText("搜索失败：索引不可用")).toBeNull();
      expect(screen.getByText("打开失败：会话不可读")).toBeInTheDocument();
      expect(onSelect).toHaveBeenCalledOnce();
      await act(async () => { fireEvent.click(screen.getByRole("button", { name: "重试打开" })); });
      expect(onSelect).toHaveBeenLastCalledWith("local", "/review");
      expect(onClose).toHaveBeenCalledOnce();
    } finally { vi.useRealTimers(); }
  });

  it("切换查询清除上次打开错误，重新打开弹窗也不恢复旧错误", async () => {
    useSessionsStore.getState().setIndependent([{ sessionId: "local", title: "交付复盘", cwd: "/review" }]);
    const props = { onSelect: vi.fn().mockRejectedValue(new Error("旧打开失败")), onClose: vi.fn() };
    const { rerender } = render(<SearchOverlay open {...props} />);
    fireEvent.click(screen.getByRole("button", { name: "交付复盘 /review" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("旧打开失败");
    fireEvent.change(screen.getByRole("textbox"), { target: { value: "新" } });
    expect(screen.queryByRole("alert")).toBeNull();
    expect(screen.queryByRole("button", { name: "重试打开" })).toBeNull();
    rerender(<SearchOverlay open={false} {...props} />);
    rerender(<SearchOverlay open {...props} />);
    expect(screen.queryByRole("alert")).toBeNull();
  });

  it("本地与 FTS 重复时保留带 snippet 的 FTS 结果", async () => {
    useSessionsStore.getState().setIndependent([{
      sessionId: "same",
      title: "相同会话",
      cwd: "/home",
    }]);
    vi.mocked(sessionSearchPage).mockResolvedValue(page([{
      sessionId: "same",
      title: "相同会话",
      cwd: "/home",
      snippet: "独特正文片段",
    }]));
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
    vi.mocked(sessionSearchPage).mockResolvedValue(page([
      { sessionId: "active", title: "交付复盘", cwd: "/home", snippet: "活动内容" },
      { sessionId: "archived", title: "归档复盘", cwd: "/home", snippet: "归档内容" },
    ]));

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
    vi.mocked(sessionSearchPage).mockResolvedValue(page([
      { sessionId: "parent", title: "主任务", cwd: "/home", snippet: "主任务内容" },
      { sessionId: "child", title: "子代理记录", cwd: "/home", snippet: "子代理内容" },
    ]));

    render(<SearchOverlay open onClose={vi.fn()} onSelect={vi.fn()} />);
    fireEvent.change(screen.getByRole("textbox", { name: "搜索会话标题或内容" }), {
      target: { value: "任务" },
    });

    expect(await screen.findByText("主任务内容")).toBeInTheDocument();
    expect(screen.queryByText("子代理记录")).toBeNull();
    expect(screen.queryByText("子代理内容")).toBeNull();
  });

  it("在首批全文命中均已归档时继续查找可打开的会话", async () => {
    const archived = Array.from({ length: 30 }, (_, index) => ({
      sessionId: `archived-${index}`, title: `历史任务 ${index}`, cwd: "/home", archived: true,
    }));
    const active = { sessionId: "active", title: "仍在进行的任务", cwd: "/home" };
    useSessionsStore.getState().setIndependent([...archived, active]);
    vi.mocked(sessionSearchPage)
      .mockResolvedValueOnce(page(archived, 30))
      .mockResolvedValueOnce(page([{ ...active, snippet: "正文包含验收" }]));
    const onSelect = vi.fn();
    render(<SearchOverlay open onClose={vi.fn()} onSelect={onSelect} />);
    fireEvent.change(screen.getByRole("textbox", { name: "搜索会话标题或内容" }), { target: { value: "验收" } });
    await userEvent.click((await screen.findByText("正文包含验收")).closest("button")!);
    expect(sessionSearchPage).toHaveBeenNthCalledWith(1, "验收", 0, 30);
    expect(sessionSearchPage).toHaveBeenNthCalledWith(2, "验收", 30, 30);
    expect(onSelect).toHaveBeenCalledWith("active", "/home");
    expect(screen.queryByText("历史任务 0")).toBeNull();
  });

  it("索引未就绪时提示刷新，索引完成后才显示确实无匹配", async () => {
    vi.mocked(sessionSearchPage).mockResolvedValueOnce(page([], null, true)).mockResolvedValueOnce(page());
    render(<SearchOverlay open onClose={vi.fn()} onSelect={vi.fn()} />);
    fireEvent.change(screen.getByRole("textbox", { name: "搜索会话标题或内容" }), { target: { value: "验收" } });
    expect(await screen.findByText("正在建立历史索引，全文结果可能尚未完整。")).toBeInTheDocument();
    expect(screen.queryByText("没有匹配的会话")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "刷新全文结果" }));
    expect(await screen.findByText("没有匹配的会话")).toBeInTheDocument();
    expect(sessionSearchPage).toHaveBeenNthCalledWith(2, "验收", 0, 30);
  });

  it("迟到的隐藏子代理检索结果在打开前被拦截并给出正确入口", async () => {
    vi.mocked(sessionSearchPage).mockResolvedValue(page([{
      sessionId: "late-child",
      title: "迟到的子代理",
      cwd: "/workspace",
      snippet: "内部执行记录",
    }]));
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
      let finishOld!: (hits: Awaited<ReturnType<typeof sessionSearchPage>>) => void;
      vi.mocked(sessionSearchPage)
        .mockImplementationOnce(() => new Promise((resolve) => { finishOld = resolve; }))
        .mockResolvedValueOnce(page([{ sessionId: "new", title: "新查询结果", cwd: "/new" }]));
      render(<SearchOverlay open onClose={vi.fn()} onSelect={vi.fn()} />);
      const input = screen.getByRole("textbox", { name: "搜索会话标题或内容" });
      fireEvent.change(input, { target: { value: "旧查询" } });
      await act(async () => vi.advanceTimersByTime(250));
      fireEvent.change(input, { target: { value: "新查询" } });
      await act(async () => finishOld(page([{ sessionId: "old", title: "旧查询结果", cwd: "/old" }])));
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
