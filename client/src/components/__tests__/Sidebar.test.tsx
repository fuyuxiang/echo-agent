import { afterEach, beforeEach, describe, it, expect, vi } from "vitest";
import { act, render, screen, fireEvent, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { Sidebar } from "../Sidebar";
import { useSessionsStore } from "@/stores/sessions-store";
import { useProjectsStore } from "@/stores/projects-store";
import { resetOrgSessionMirror, useOrgSessionStore } from "@/stores/org-session-store";

vi.mock("@/lib/agent-client", () => ({
  agentRenameSession: vi.fn().mockResolvedValue(undefined),
  agentDeleteSession: vi.fn().mockResolvedValue(undefined),
  agentSetSessionPinned: vi.fn((_id: string, pinned: boolean) => Promise.resolve(pinned)),
  agentSetSessionArchived: vi.fn((_id: string, archived: boolean) => Promise.resolve(archived)),
  projectsLoad: vi.fn().mockResolvedValue([]),
  projectsSave: vi.fn().mockResolvedValue(undefined),
}));

const base = {
  onNewSession: vi.fn(),
  onSelect: vi.fn(),
  onNavigate: vi.fn(),
  onOpenSettings: vi.fn(),
  onToggleCollapse: vi.fn(),
  onOpenSearch: vi.fn(),
  onPlaceholder: vi.fn(),
  activeNav: "新建任务",
};

describe("Sidebar", () => {
  afterEach(() => vi.useRealTimers());

  beforeEach(() => {
    resetOrgSessionMirror();
    useSessionsStore.setState({
      independent: [],
      workspaces: [],
      tasksOpen: true,
      projectsOpen: true,
      homeCwd: "/home",
      currentSessionId: null,
      loading: false,
      error: null,
      query: "",
      filterStatus: null,
      filterDate: null,
      pendingSessionPatches: {},
      drafts: {},
    });
    useProjectsStore.setState({
      projects: [],
      activeProjectId: null,
      persisting: false,
      persistError: null,
    });
  });

  it("按工作流顺序渲染导航项", () => {
    const { container } = render(<Sidebar {...base} />);
    const nav = container.querySelector(".sidebar__nav");
    expect(nav).not.toBeNull();
    expect(within(nav as HTMLElement).getAllByRole("button").map((button) => button.textContent?.trim())).toEqual([
      "新建任务", "项目", "组织", "扩展", "代码开发", "更多知识与工具",
    ]);
    expect(screen.queryByText("助理")).not.toBeInTheDocument();
  });

  it("点击占位导航触发 onNavigate", () => {
    const onNavigate = vi.fn();
    render(<Sidebar {...base} onNavigate={onNavigate} />);
    fireEvent.click(screen.getByText("项目"));
    expect(onNavigate).toHaveBeenCalledWith("项目");
  });

  it("组织是直接可见的入口并进入组织页面", () => {
    const onNavigate = vi.fn();
    render(<Sidebar {...base} activeNav="组织" onNavigate={onNavigate} />);
    const entry = screen.getByRole("button", { name: "组织" });
    expect(entry).toHaveClass("sidebar__nav-item--active");
    fireEvent.click(entry);
    expect(onNavigate).toHaveBeenCalledWith("组织");
  });

  it("渲染会话列表并可选中", () => {
    const onSelect = vi.fn();
    // cwd-less session ⇒ 任务 group (independent). onSelect now also receives cwd.
    useSessionsStore.getState().setIndependent([{ sessionId: "s1", title: "测试会话", cwd: "" } as any]);
    render(<Sidebar {...base} onSelect={onSelect} />);
    fireEvent.click(screen.getByText("测试会话"));
    expect(onSelect).toHaveBeenCalledWith("s1", "");
  });

  it("不把隐藏子代理会话当作顶层任务", () => {
    useSessionsStore.getState().setIndependent([
      { sessionId: "parent", title: "主任务", cwd: "/workspace", hidden: false },
      { sessionId: "child", title: "子代理内部会话", cwd: "/workspace", hidden: true, sessionKind: "subagent" },
    ]);

    render(<Sidebar {...base} />);
    expect(screen.getByText("主任务")).toBeInTheDocument();
    expect(screen.queryByText("子代理内部会话")).toBeNull();
  });

  it("在侧栏精确展示运行、等待用户、未开始、停止与失败", () => {
    useSessionsStore.getState().setIndependent([
      { sessionId: "running", title: "后台分析", cwd: "", status: "working" },
      { sessionId: "permission", title: "等待确认", cwd: "", status: "awaiting_permission" },
      { sessionId: "answer", title: "等待选项", cwd: "", status: "awaiting_answer" },
      { sessionId: "approval", title: "等待方案", cwd: "", status: "awaiting_approval" },
      { sessionId: "pending", title: "新建任务", cwd: "", status: "pending" },
      { sessionId: "stopped", title: "已取消任务", cwd: "", status: "stopped" },
      { sessionId: "failed", title: "失败任务", cwd: "", status: "failed" },
      { sessionId: "done", title: "普通历史", cwd: "", status: "completed" },
    ]);

    render(<Sidebar {...base} />);

    expect(screen.getByText("运行中")).toBeInTheDocument();
    expect(screen.getByText("授权待确认")).toBeInTheDocument();
    expect(screen.getByText("等待回答")).toBeInTheDocument();
    expect(screen.getByText("方案待批准")).toBeInTheDocument();
    expect(screen.getByText("未开始")).toBeInTheDocument();
    expect(screen.getByText("已停止")).toBeInTheDocument();
    expect(screen.getByText("失败")).toBeInTheDocument();
    expect(screen.getByText("普通历史").closest(".sidebar__conv"))
      .not.toHaveTextContent("已完成");
  });

  it("搜索按钮触发 onOpenSearch,设置按钮触发 onOpenSettings", () => {
    const onOpenSearch = vi.fn();
    const onOpenSettings = vi.fn();
    render(
      <Sidebar {...base} onOpenSearch={onOpenSearch} onOpenSettings={onOpenSettings} />
    );
    fireEvent.click(screen.getByRole("button", { name: "搜索" }));
    expect(onOpenSearch).toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "设置" }));
    expect(onOpenSettings).toHaveBeenCalled();
  });

  it("通知与用户入口走各自的真实路由", () => {
    const onPlaceholder = vi.fn();
    const onOpenSettings = vi.fn();
    render(<Sidebar {...base} onPlaceholder={onPlaceholder} onOpenSettings={onOpenSettings} />);
    fireEvent.click(screen.getByRole("button", { name: "通知" }));
    fireEvent.click(screen.getByRole("button", { name: "本地用户" }));
    expect(onPlaceholder).toHaveBeenNthCalledWith(1, "通知");
    expect(onPlaceholder).toHaveBeenNthCalledWith(2, "用户中心");
    expect(onOpenSettings).not.toHaveBeenCalled();
  });

  it("左下角本地用户与操作按钮保持同一行", () => {
    render(<Sidebar {...base} />);
    const user = screen.getByRole("button", { name: "本地用户" });
    const footer = user.closest(".sidebar__footer");

    expect(user.querySelector(".sidebar__user-label")).toHaveTextContent("本地用户");
    expect(footer?.querySelector(".sidebar__logo-spacer")).toBeNull();
    expect(footer?.children).toHaveLength(3);
  });

  it("登录组织后左下角显示组织用户名", () => {
    useOrgSessionStore.getState().setSession({
      loggedIn: true,
      serverUrl: "https://memory.example.com",
      user: {
        id: "u1",
        username: "alice",
        displayName: "Alice Zhang",
        role: "member",
        clearance: 1,
      },
    });

    render(<Sidebar {...base} />);

    const user = screen.getByRole("button", { name: "Alice Zhang" });
    expect(user).toHaveTextContent("Alice Zhang");
    expect(user).toHaveAttribute("title", "Alice Zhang · https://memory.example.com");
  });

  it("收起侧边栏按钮触发 onToggleCollapse", () => {
    const onToggleCollapse = vi.fn();
    render(<Sidebar {...base} onToggleCollapse={onToggleCollapse} />);
    fireEvent.click(screen.getByRole("button", { name: "收起侧边栏" }));
    expect(onToggleCollapse).toHaveBeenCalled();
  });

  it("任务与项目折叠按钮暴露 aria-expanded 状态", () => {
    render(<Sidebar {...base} />);
    const tasks = screen.getByRole("button", { name: /^\u4efb\u52a1/ });
    const projects = screen.getByRole("button", { name: /^\u9879\u76ee \(/ });

    expect(tasks).toHaveAttribute("aria-expanded", "true");
    expect(projects).toHaveAttribute("aria-expanded", "true");
    fireEvent.click(tasks);
    fireEvent.click(projects);
    expect(tasks).toHaveAttribute("aria-expanded", "false");
    expect(projects).toHaveAttribute("aria-expanded", "false");
  });

  it("升级后旧默认目录会话仍显示为任务且不生成用户名空间", () => {
    useSessionsStore.setState({
      independent: [{ sessionId: "legacy", title: "升级前任务", cwd: "/Users/demo" }],
      workspaces: [{ cwd: "/Users/demo", sessionCount: 1 }],
      homeCwd: "/Users/demo/Documents/EchoAgent",
    });

    render(<Sidebar {...base} />);

    expect(screen.getByText("升级前任务")).toBeInTheDocument();
    expect(screen.queryByText("demo")).toBeNull();
    expect(screen.getByRole("button", { name: "项目 (0)" })).toBeInTheDocument();
  });

  it("项目会话只在其真实项目下展示", () => {
    useSessionsStore.setState({
      independent: [{ sessionId: "project-session", title: "项目对话", cwd: "/workspace" }],
    });
    useProjectsStore.setState({
      projects: [{
        id: "project-1",
        name: "客户项目",
        cwd: "/workspace",
        createdAt: "2026-09-05T00:00:00.000Z",
        connectors: [], experts: [], skills: [], plans: [], tasks: [], assets: [], members: [],
        conversations: [{
          sessionId: "project-session",
          title: "项目对话",
          createdAt: "2026-09-05T00:00:00.000Z",
        }],
      }],
    });

    render(<Sidebar {...base} />);

    expect(screen.queryByText("项目对话")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "展开客户项目对话" }));
    expect(screen.getByText("项目对话")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /^\u4efb\u52a1/ })).toHaveTextContent("任务 (0)");
  });

  it("任务会话可选择项目移入，保留原工作目录和会话记录", async () => {
    useSessionsStore.getState().setIndependent([
      { sessionId: "task-session", title: "需求探索", cwd: "/original", currentModelId: "model-a" },
    ]);
    useProjectsStore.setState({
      projects: [{
        id: "project-1", name: "客户项目", cwd: "/project", createdAt: "2026-09-05T00:00:00.000Z",
        connectors: [], experts: [], skills: [], plans: [], tasks: [], assets: [], members: [], conversations: [],
      }],
    });
    render(<Sidebar {...base} />);
    fireEvent.click(screen.getByRole("button", { name: "需求探索的会话操作" }));
    fireEvent.click(screen.getByRole("menuitem", { name: "移入项目…" }));

    const dialog = screen.getByRole("dialog", { name: "移入项目" });
    fireEvent.click(within(dialog).getByRole("radio", { name: "客户项目" }));
    expect(dialog).toHaveTextContent("仍在原工作目录运行");
    fireEvent.click(within(dialog).getByRole("button", { name: "移入项目" }));

    await waitFor(() => expect(screen.queryByRole("dialog", { name: "移入项目" })).toBeNull());
    expect(screen.getByRole("button", { name: /^\u4efb\u52a1/ })).toHaveTextContent("任务 (0)");
    expect(screen.getByText("需求探索")).toBeInTheDocument();
    expect(screen.getByText("需求探索").closest(".sidebar__conv")?.querySelector("button")).toHaveFocus();
    expect(useSessionsStore.getState().independent[0].cwd).toBe("/original");
    expect(useProjectsStore.getState().projects[0].conversations[0]).toMatchObject({
      sessionId: "task-session", modelId: "model-a", pendingProjectContext: true,
    });
  });

  it("没有项目时从移入弹窗前往项目页创建", () => {
    useSessionsStore.getState().setIndependent([{ sessionId: "s1", title: "待整理", cwd: "/workspace" }]);
    const onNavigate = vi.fn();
    render(<Sidebar {...base} onNavigate={onNavigate} />);
    fireEvent.click(screen.getByRole("button", { name: "待整理的会话操作" }));
    fireEvent.click(screen.getByRole("menuitem", { name: "移入项目…" }));
    fireEvent.click(screen.getByRole("button", { name: "前往项目页创建" }));
    expect(onNavigate).toHaveBeenCalledWith("项目");
    expect(screen.queryByRole("dialog", { name: "移入项目" })).toBeNull();
  });

  it("侧栏项目对话提供移出、归档和经确认的永久删除", async () => {
    useSessionsStore.setState({
      independent: [{ sessionId: "project-session", title: "过期对话", cwd: "/workspace" }],
    });
    useProjectsStore.setState({
      projects: [{
        id: "project-1",
        name: "客户项目",
        cwd: "/workspace",
        createdAt: "2026-09-05T00:00:00.000Z",
        connectors: [], experts: [], skills: [], plans: [], tasks: [], assets: [], members: [],
        conversations: [{
          sessionId: "project-session",
          title: "过期对话",
          createdAt: "2026-09-05T00:00:00.000Z",
        }],
      }],
    });
    const onDeleteSession = vi.fn().mockResolvedValue(undefined);
    render(<Sidebar {...base} onDeleteSession={onDeleteSession} />);
    fireEvent.click(screen.getByRole("button", { name: "展开客户项目对话" }));
    fireEvent.click(screen.getByRole("button", { name: "过期对话的会话操作" }));

    const menu = screen.getByRole("menu", { name: "过期对话 会话操作" });
    expect(within(menu).getByRole("menuitem", { name: "归档" })).toBeInTheDocument();
    expect(within(menu).getByRole("menuitem", { name: "移出项目" })).toBeInTheDocument();
    fireEvent.click(within(menu).getByRole("menuitem", { name: "永久删除" }));

    expect(onDeleteSession).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "永久删除" }));
    await waitFor(() => expect(onDeleteSession).toHaveBeenCalledWith("project-session", "/workspace"));
  });

  it("悬停「更多」自动展开，只展示知识库和定时任务且不抢走焦点", async () => {
    const onNavigate = vi.fn();
    const user = userEvent.setup();
    render(<Sidebar {...base} onNavigate={onNavigate} />);
    const settings = screen.getByRole("button", { name: "设置" });
    settings.focus();
    await user.hover(screen.getByRole("button", { name: /更多/ }));
    const menu = screen.getByRole("menu");
    expect(menu).toBeInTheDocument();
    expect(settings).toHaveFocus();
    expect(onNavigate).not.toHaveBeenCalled();
    expect(within(menu).getByText("知识库")).toBeInTheDocument();
    expect(within(menu).getByRole("menuitem", { name: "定时任务" })).toBeInTheDocument();
    expect(within(menu).getAllByRole("menuitem")).toHaveLength(2);
    for (const settingsEntry of ["我的文件", "个人记忆", "用量统计", "通知渠道", "云存储", "代码开发"]) {
      expect(within(menu).queryByText(settingsEntry)).not.toBeInTheDocument();
    }
    for (const removed of ["灵感", "网页预览", "策略设置", "发现"]) {
      expect(within(menu).queryByText(removed)).not.toBeInTheDocument();
    }
  });

  it("悬停后再点击「更多」保持菜单展开", async () => {
    const user = userEvent.setup();
    render(<Sidebar {...base} />);
    const trigger = screen.getByRole("button", { name: /更多/ });

    await user.hover(trigger);
    await user.click(trigger);

    expect(screen.getByRole("menu", { name: "更多功能" })).toBeInTheDocument();
    expect(trigger).toHaveAttribute("aria-expanded", "true");
  });

  it("鼠标跨过按钮与弹层间隙时保持展开，离开菜单后延迟关闭", () => {
    vi.useFakeTimers();
    render(<Sidebar {...base} />);
    const trigger = screen.getByRole("button", { name: /更多/ });
    fireEvent.mouseEnter(trigger);
    const menu = screen.getByRole("menu", { name: "更多功能" });

    fireEvent.mouseLeave(trigger, { relatedTarget: document.body });
    act(() => vi.advanceTimersByTime(60));
    expect(menu).toBeInTheDocument();
    fireEvent.mouseEnter(menu, { relatedTarget: document.body });
    act(() => vi.advanceTimersByTime(120));
    expect(menu).toBeInTheDocument();

    fireEvent.mouseLeave(menu, { relatedTarget: document.body });
    act(() => vi.advanceTimersByTime(119));
    expect(menu).toBeInTheDocument();
    act(() => vi.advanceTimersByTime(1));
    expect(screen.queryByRole("menu", { name: "更多功能" })).toBeNull();
    expect(trigger).toHaveAttribute("aria-expanded", "false");
  });

  it("鼠标离开不会关闭正在用键盘操作的菜单，焦点离开后关闭", () => {
    vi.useFakeTimers();
    render(<Sidebar {...base} />);
    const trigger = screen.getByRole("button", { name: /更多/ });
    trigger.focus();
    fireEvent.keyDown(trigger, { key: "ArrowUp" });
    const menu = screen.getByRole("menu", { name: "更多功能" });
    const lastItem = within(menu).getByRole("menuitem", { name: "定时任务" });
    expect(lastItem).toHaveFocus();

    fireEvent.mouseLeave(menu, { relatedTarget: document.body });
    act(() => vi.advanceTimersByTime(120));
    expect(menu).toBeInTheDocument();
    expect(lastItem).toHaveFocus();

    act(() => screen.getByRole("button", { name: "设置" }).focus());
    expect(screen.queryByRole("menu", { name: "更多功能" })).toBeNull();
  });

  it("悬停展开后可用 Escape 或点击外部关闭", async () => {
    const user = userEvent.setup();
    render(<Sidebar {...base} />);
    const trigger = screen.getByRole("button", { name: /更多/ });

    await user.hover(trigger);
    await user.keyboard("{Escape}");
    expect(screen.queryByRole("menu", { name: "更多功能" })).toBeNull();
    expect(trigger).toHaveFocus();

    await user.unhover(trigger);
    await user.hover(trigger);
    expect(screen.getByRole("menu", { name: "更多功能" })).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "设置" }));
    expect(screen.queryByRole("menu", { name: "更多功能" })).toBeNull();
  });

  it("无悬停操作时仍可点击打开菜单", async () => {
    const user = userEvent.setup({ skipHover: true });
    render(<Sidebar {...base} />);

    await user.click(screen.getByRole("button", { name: /更多/ }));
    expect(screen.getByRole("menu", { name: "更多功能" })).toBeInTheDocument();
    await user.click(screen.getByRole("menuitem", { name: "知识库" }));
    expect(screen.queryByRole("menu", { name: "更多功能" })).toBeNull();
  });

  it.each(["专家·技能·连接器", "技能", "连接器", "插件·市场", "插件市场"])("扩展子页 %s 始终选中同一个侧栏入口", activeNav => {
    render(<Sidebar {...base} activeNav={activeNav} />);
    expect(screen.getByRole("button", { name: "扩展" })).toHaveClass("sidebar__nav-item--active");
    expect(screen.getByText("更多").closest("button")).not.toHaveClass("sidebar__nav-item--active");
  });

  it("「更多」菜单可进入知识库", async () => {
    const onNavigate = vi.fn();
    const user = userEvent.setup();
    render(<Sidebar {...base} onNavigate={onNavigate} onToast={vi.fn()} />);
    await user.hover(screen.getByRole("button", { name: /更多/ }));
    const menu = await screen.findByRole("menu");
    expect(menu).toBeInTheDocument();
    fireEvent.click(screen.getByText("知识库"));
    expect(onNavigate).toHaveBeenCalledWith("知识库");
  });

  it("「更多」中的定时任务进入自动化页面并显示选中状态", () => {
    const onNavigate = vi.fn();
    const { rerender } = render(<Sidebar {...base} onNavigate={onNavigate} />);
    const trigger = screen.getByRole("button", { name: /更多/ });
    fireEvent.click(trigger);
    fireEvent.click(screen.getByRole("menuitem", { name: "定时任务" }));
    expect(onNavigate).toHaveBeenCalledWith("自动化");
    expect(screen.queryByRole("menu", { name: "更多功能" })).toBeNull();

    rerender(<Sidebar {...base} activeNav="自动化" onNavigate={onNavigate} />);
    expect(trigger).toHaveClass("sidebar__nav-item--active");
    fireEvent.click(trigger);
    expect(screen.getByRole("menuitem", { name: "定时任务" })).toHaveClass("sidebar__more-item--active");
  });

  it("主导航可进入代码开发工作台", () => {
    const onNavigate = vi.fn();
    render(<Sidebar {...base} onNavigate={onNavigate} />);

    fireEvent.click(screen.getByText("代码开发"));

    expect(onNavigate).toHaveBeenCalledWith("代码开发");
  });

  it("「更多」菜单支持方向键、Home/End 和 Escape 焦点回退", async () => {
    const user = userEvent.setup();
    render(<Sidebar {...base} />);
    const trigger = screen.getByRole("button", { name: /更多/ });
    trigger.focus();

    await user.keyboard("{ArrowDown}");
    const menu = screen.getByRole("menu", { name: "更多功能" });
    const items = within(menu).getAllByRole("menuitem");
    expect(items[0]).toHaveFocus();

    await user.keyboard("{ArrowDown}");
    expect(items[1]).toHaveFocus();
    await user.keyboard("{End}");
    expect(items[items.length - 1]).toHaveFocus();
    await user.keyboard("{Home}");
    expect(items[0]).toHaveFocus();
    await user.keyboard("{Escape}");

    expect(screen.queryByRole("menu", { name: "更多功能" })).toBeNull();
    expect(trigger).toHaveFocus();
  });

  it("任务筛选菜单可用方向键打开、循环导航并用 Escape 关闭", async () => {
    const user = userEvent.setup();
    render(<Sidebar {...base} />);
    const trigger = screen.getByRole("button", { name: "筛选任务" });
    trigger.focus();

    await user.keyboard("{ArrowUp}");
    const menu = screen.getByRole("menu", { name: "任务筛选" });
    const items = within(menu).getAllByRole("menuitemradio");
    expect(items[items.length - 1]).toHaveFocus();
    await user.keyboard("{ArrowDown}");
    expect(items[0]).toHaveFocus();
    await user.keyboard("{Escape}");

    expect(screen.queryByRole("menu", { name: "任务筛选" })).toBeNull();
    expect(trigger).toHaveFocus();
  });

  it("归档会话始终从侧边栏收起且不再提供归档范围筛选", () => {
    useSessionsStore.getState().setIndependent([
      { sessionId: "active", title: "活动会话", cwd: "/home", archived: false },
      { sessionId: "archived", title: "归档会话", cwd: "/home", archived: true },
    ]);
    render(<Sidebar {...base} />);
    expect(screen.getByText("活动会话")).toBeInTheDocument();
    expect(screen.queryByText("归档会话")).toBeNull();

    fireEvent.click(screen.getByRole("button", { name: "筛选任务" }));
    expect(screen.queryByRole("menuitemradio", { name: "已归档会话" })).toBeNull();
    expect(screen.getByText("活动会话")).toBeInTheDocument();
    expect(screen.queryByText("归档会话")).toBeNull();
  });

  it("项目会话的主记录已归档时也不在侧边栏残留", () => {
    useSessionsStore.getState().setIndependent([
      { sessionId: "project-archived", title: "已收起项目对话", cwd: "/workspace", archived: true },
    ]);
    useProjectsStore.setState({
      projects: [{
        id: "project-1",
        name: "客户项目",
        cwd: "/workspace",
        createdAt: "2026-09-05T00:00:00.000Z",
        connectors: [], experts: [], skills: [], plans: [], tasks: [], assets: [], members: [],
        // Simulate legacy/stale project metadata: the authoritative session
        // record is archived even though the duplicated relation flag is not.
        conversations: [{
          sessionId: "project-archived",
          title: "已收起项目对话",
          createdAt: "2026-09-05T00:00:00.000Z",
          archived: false,
        }],
      }],
    });

    render(<Sidebar {...base} />);
    fireEvent.click(screen.getByRole("button", { name: "展开客户项目对话" }));

    expect(screen.queryByText("已收起项目对话")).toBeNull();
    expect(screen.getByText("还没有对话，从项目页开启第一轮吧")).toBeInTheDocument();
  });

  it("项目归档镜像陈旧时以主会话的恢复状态为准", () => {
    useSessionsStore.getState().setIndependent([
      { sessionId: "project-restored", title: "已恢复项目对话", cwd: "/workspace", archived: false },
    ]);
    useProjectsStore.setState({
      projects: [{
        id: "project-1",
        name: "客户项目",
        cwd: "/workspace",
        createdAt: "2026-09-05T00:00:00.000Z",
        connectors: [], experts: [], skills: [], plans: [], tasks: [], assets: [], members: [],
        conversations: [{
          sessionId: "project-restored",
          title: "已恢复项目对话",
          createdAt: "2026-09-05T00:00:00.000Z",
          archived: true,
        }],
      }],
    });

    render(<Sidebar {...base} />);
    fireEvent.click(screen.getByRole("button", { name: "展开客户项目对话" }));
    expect(screen.getByText("已恢复项目对话")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "已恢复项目对话的会话操作" }));
    expect(screen.getByRole("menuitem", { name: "归档" })).toBeInTheDocument();
  });
});
