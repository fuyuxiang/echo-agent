import { afterEach, beforeEach, describe, it, expect, vi } from "vitest";
import { act, render, screen, fireEvent, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { Sidebar } from "../Sidebar";
import { useSessionsStore } from "@/stores/sessions-store";
import { useProjectsStore } from "@/stores/projects-store";
import { resetOrgSessionMirror, useOrgSessionStore } from "@/stores/org-session-store";
import { SidebarLayoutContext } from "@/lib/sidebar-layout";
import { SIDEBAR_MENU_STORAGE_KEY } from "@/lib/sidebar-menu";
import type { SessionStatus } from "@/lib/types";
import { agentDeleteSession } from "@/lib/agent-client";

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
    localStorage.removeItem(SIDEBAR_MENU_STORAGE_KEY);
    resetOrgSessionMirror();
    useSessionsStore.setState({ deletedSessionIds: [],
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
      "新建任务", "项目", "组织", "代码开发", "专家·技能·连接器", "更多知识与工具",
    ]);
    expect(screen.queryByText("助理")).not.toBeInTheDocument();
  });

  it.each(["collapsed", "resizing"] as const)("侧栏 %s 时关闭更多浮层并阻止重新展开", async (state) => {
    const layout = { collapsed: false, resizing: false, panelMaxWidth: 600, registerPanel: vi.fn(), expandSidebar: vi.fn(), toggleSidebar: vi.fn() };
    const { rerender } = render(<SidebarLayoutContext.Provider value={layout}><Sidebar {...base} /></SidebarLayoutContext.Provider>);
    await userEvent.hover(screen.getByRole("button", { name: /更多/ }));
    expect(screen.getByRole("menu", { name: "更多功能" })).toBeInTheDocument();
    rerender(<SidebarLayoutContext.Provider value={{ ...layout, [state]: true }}><Sidebar {...base} /></SidebarLayoutContext.Provider>);
    expect(screen.queryByRole("menu", { name: "更多功能" })).not.toBeInTheDocument();
    fireEvent.mouseEnter(screen.getByRole("button", { name: /更多/ }));
    fireEvent.keyDown(screen.getByRole("button", { name: /更多/ }), { key: "ArrowDown" });
    expect(screen.queryByRole("menu", { name: "更多功能" })).not.toBeInTheDocument();
  });

  it("点击占位导航触发 onNavigate", () => {
    const onNavigate = vi.fn();
    render(<Sidebar {...base} onNavigate={onNavigate} />);
    fireEvent.click(screen.getByText("项目"));
    expect(onNavigate).toHaveBeenCalledWith("项目");
  });

  it("组织可从主导航进入并显示选中状态", () => {
    const onNavigate = vi.fn();
    render(<Sidebar {...base} activeNav="组织" onNavigate={onNavigate} />);
    const entry = screen.getByRole("button", { name: "组织" });
    expect(entry).toHaveClass("sidebar__nav-item--active");
    expect(screen.getByRole("button", { name: /更多/ })).not.toHaveClass("sidebar__nav-item--active");
    fireEvent.click(entry);
    expect(onNavigate).toHaveBeenCalledWith("组织");
  });

  const openMenuCustomizer = () => {
    fireEvent.click(screen.getByRole("button", { name: /更多/ }));
    fireEvent.click(screen.getByRole("menuitem", { name: "自定义菜单栏" }));
    return screen.getByRole("dialog", { name: "自定义菜单栏" });
  };

  const moveMenuEntry = (dialog: HTMLElement, label: string, key: string) => {
    fireEvent.keyDown(within(dialog).getByRole("button", { name: `拖动${label}` }), { key });
  };

  const measureMenu = (dialog: HTMLElement) => {
    const rect = (top: number, height: number) => ({ left: 0, right: 500, top, bottom: top + height, width: 500, height, x: 0, y: top, toJSON: () => ({}) });
    vi.spyOn(dialog.querySelector(".sidebar-menu-customizer__body")!, "getBoundingClientRect").mockImplementation(() => rect(0, 800));
    for (const section of dialog.querySelectorAll<HTMLElement>("[data-menu-group]")) {
      const top = section.dataset.menuGroup === "primary" ? 0 : 440;
      vi.spyOn(section, "getBoundingClientRect").mockImplementation(() => rect(top, 360));
      for (const row of section.querySelectorAll<HTMLElement>("[data-menu-id]")) {
        vi.spyOn(row, "getBoundingClientRect").mockImplementation(() => rect(top + 80 + [...section.querySelectorAll("[data-menu-id]")].indexOf(row) * 60, 50));
      }
    }
  };
  const menuPointer = (target: Element | Window, type: string, y: number, x = 460, id = 1, buttons = type === "pointerup" ? 0 : 1) => {
    const event = new MouseEvent(type, { bubbles: true, cancelable: true, clientX: x, clientY: y, button: 0, buttons });
    Object.defineProperties(event, { pointerId: { value: id }, pointerType: { value: "mouse" } });
    fireEvent(target, event);
  };
  const dragMenuEntry = async (dialog: HTMLElement, label: string, y: number) => {
    measureMenu(dialog);
    const handle = within(dialog).getByRole("button", { name: `拖动${label}` });
    const source = handle.closest("[data-menu-id]")!.getBoundingClientRect();
    menuPointer(handle, "pointerdown", source.top + 25);
    menuPointer(window, "pointermove", y);
    await act(async () => { await new Promise(resolve => setTimeout(resolve, 25)); });
    menuPointer(window, "pointerup", y);
    await act(async () => { await new Promise(resolve => setTimeout(resolve, 0)); });
  };

  it("自定义菜单支持跨组移动、排序、保存和重新挂载后恢复", () => {
    const onNavigate = vi.fn();
    const { container, unmount } = render(<Sidebar {...base} onNavigate={onNavigate} />);
    const dialog = openMenuCustomizer();
    expect(screen.queryByRole("menu", { name: "更多功能" })).not.toBeInTheDocument();
    expect(within(dialog).getByText("新建任务").closest("div")).not.toHaveAttribute("draggable");
    expect(within(dialog).queryByRole("button", { name: /^(上移|下移|将)/ })).not.toBeInTheDocument();
    expect(within(dialog).getByRole("button", { name: "拖动项目" })).toHaveAttribute("draggable", "false");
    moveMenuEntry(dialog, "知识库", "ArrowLeft");
    moveMenuEntry(dialog, "知识库", "ArrowUp");
    moveMenuEntry(dialog, "组织", "ArrowRight");
    expect(within(dialog).getByRole("button", { name: "拖动组织" })).toHaveFocus();
    fireEvent.click(within(dialog).getByRole("button", { name: "保存" }));
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    expect(within(container.querySelector(".sidebar__nav") as HTMLElement).getAllByRole("button").map(el => el.textContent)).toEqual([
      "新建任务", "项目", "代码开发", "知识库", "专家·技能·连接器", "更多",
    ]);
    fireEvent.click(screen.getByRole("button", { name: "知识库" }));
    expect(onNavigate).toHaveBeenCalledWith("知识库");
    unmount();
    render(<Sidebar {...base} activeNav="组织" onNavigate={onNavigate} />);
    expect(screen.getByRole("button", { name: "知识库" })).toBeInTheDocument();
    const more = screen.getByRole("button", { name: "更多" });
    expect(more).toHaveClass("sidebar__nav-item--active");
    fireEvent.click(more);
    fireEvent.click(screen.getByRole("menuitem", { name: "组织" }));
    expect(onNavigate).toHaveBeenCalledWith("组织");
  });

  it("不依赖 HTML5 拖放事件，指针拖动支持跨组移动并调整同组顺序", async () => {
    render(<Sidebar {...base} />);
    const dialog = openMenuCustomizer();
    const primary = within(dialog).getByRole("list", { name: "一级菜单入口" });
    await dragMenuEntry(dialog, "知识库", 82);
    expect(Array.from(primary.querySelectorAll("[data-menu-id]")).map(el => el.getAttribute("data-menu-id"))).toEqual([
      "knowledge", "projects", "organization", "coding", "capabilities",
    ]);
    await dragMenuEntry(dialog, "知识库", 247);
    fireEvent.click(within(dialog).getByRole("button", { name: "保存" }));
    expect(JSON.parse(localStorage.getItem(SIDEBAR_MENU_STORAGE_KEY)!).primary).toEqual([
      "projects", "organization", "knowledge", "coding", "capabilities",
    ]);
  });

  it.each(["pointercancel", "blur", "Escape", "lostpointercapture", "outside"])("指针拖动在 %s 时取消且不误关弹窗", async reason => {
    render(<Sidebar {...base} />);
    const dialog = openMenuCustomizer();
    measureMenu(dialog);
    const handle = within(dialog).getByRole("button", { name: "拖动项目" });
    menuPointer(handle, "pointerdown", 105);
    menuPointer(window, "pointermove", 300);
    await act(async () => { await new Promise(resolve => setTimeout(resolve, 25)); });
    expect(document.querySelector(".sidebar-menu-customizer__preview")).toBeInTheDocument();
    if (reason === "Escape") fireEvent.keyDown(window, { key: "Escape" });
    else if (reason === "blur") fireEvent(window, new Event("blur"));
    else if (reason === "outside") menuPointer(window, "pointerup", 300, 900);
    else menuPointer(reason === "lostpointercapture" ? handle : window, reason, 300);
    expect(document.querySelector(".sidebar-menu-customizer__preview")).not.toBeInTheDocument();
    expect(screen.getByRole("dialog", { name: "自定义菜单栏" })).toBeInTheDocument();
    expect(within(dialog).getByRole("list", { name: "一级菜单入口" }).querySelector("[data-menu-id]")).toHaveAttribute("data-menu-id", "projects");
    expect(localStorage.getItem(SIDEBAR_MENU_STORAGE_KEY)).toBeNull();
  });

  it("原生事件未提供全局 buttons 状态时仍按捕获的按下和松开完成拖动", async () => {
    render(<Sidebar {...base} />);
    const dialog = openMenuCustomizer();
    measureMenu(dialog);
    menuPointer(within(dialog).getByRole("button", { name: "拖动项目" }), "pointerdown", 105, 460, 1, 0);
    menuPointer(window, "pointermove", 247, 460, 1, 0);
    menuPointer(window, "pointerup", 247, 460, 1, 0);
    expect([...within(dialog).getByRole("list", { name: "一级菜单入口" }).querySelectorAll<HTMLElement>("[data-menu-id]")].map(row => row.dataset.menuId)).toEqual([
      "organization", "coding", "projects", "capabilities",
    ]);
    await act(async () => { await new Promise(resolve => setTimeout(resolve, 0)); });
  });

  it("取消或 Escape 放弃草稿并将焦点还给更多按钮", async () => {
    render(<Sidebar {...base} />);
    let dialog = openMenuCustomizer();
    moveMenuEntry(dialog, "项目", "ArrowRight");
    fireEvent.click(within(dialog).getByRole("button", { name: "取消" }));
    expect(screen.getByRole("button", { name: "项目" })).toBeInTheDocument();
    dialog = openMenuCustomizer();
    moveMenuEntry(dialog, "项目", "ArrowRight");
    await userEvent.keyboard("{Escape}");
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: /更多/ })).toHaveFocus();
    expect(localStorage.getItem(SIDEBAR_MENU_STORAGE_KEY)).toBeNull();
  });

  it("所有入口放进一级菜单后仍能自定义、正确导航并恢复默认", () => {
    const onNavigate = vi.fn();
    const { rerender } = render(<Sidebar {...base} onNavigate={onNavigate} />);
    let dialog = openMenuCustomizer();
    for (const label of ["知识库", "定时任务"]) moveMenuEntry(dialog, label, "ArrowLeft");
    expect(within(dialog).getByText("将入口拖到这里")).toBeInTheDocument();
    fireEvent.click(within(dialog).getByRole("button", { name: "保存" }));
    fireEvent.click(screen.getByRole("button", { name: "定时任务" }));
    expect(onNavigate).toHaveBeenCalledWith("自动化");
    rerender(<Sidebar {...base} activeNav="自动化" onNavigate={onNavigate} />);
    expect(screen.getByRole("button", { name: "定时任务" })).toHaveClass("sidebar__nav-item--active");
    fireEvent.click(screen.getByRole("button", { name: "更多" }));
    expect(screen.getAllByRole("menuitem")).toHaveLength(1);
    fireEvent.click(screen.getByRole("menuitem", { name: "自定义菜单栏" }));
    dialog = screen.getByRole("dialog", { name: "自定义菜单栏" });
    fireEvent.click(within(dialog).getByRole("button", { name: "恢复默认" }));
    fireEvent.click(within(dialog).getByRole("button", { name: "保存" }));
    expect(screen.queryByRole("button", { name: "定时任务" })).not.toBeInTheDocument();
    expect(JSON.parse(localStorage.getItem(SIDEBAR_MENU_STORAGE_KEY)!).more).toEqual(["knowledge", "automation"]);
  });

  it("保存失败时保留草稿并提示重试，不应用未保存的菜单", () => {
    render(<Sidebar {...base} />);
    const dialog = openMenuCustomizer();
    moveMenuEntry(dialog, "项目", "ArrowRight");
    const failSave = vi.spyOn(Storage.prototype, "setItem").mockImplementation(() => { throw new Error("quota"); });
    try {
      fireEvent.click(within(dialog).getByRole("button", { name: "保存" }));
      expect(within(dialog).getByRole("alert")).toHaveTextContent("菜单设置保存失败");
      expect(screen.getByRole("button", { name: "项目" })).toBeInTheDocument();
      expect(within(within(dialog).getByRole("list", { name: "更多入口" })).getByRole("button", { name: "拖动项目" })).toBeInTheDocument();
    } finally { failSave.mockRestore(); }
    fireEvent.click(within(dialog).getByRole("button", { name: "保存" }));
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "项目" })).not.toBeInTheDocument();
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

  it("直接显示最后更新时间，按分钟刷新并在重新聚焦后追上实际时间", () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-10-18T10:05:00Z"));
    useSessionsStore.getState().setIndependent([
      { sessionId: "recent", title: "数字会话标识", cwd: "", updatedAt: "2026-10-18T10:00:00Z" },
    ]);
    const { container } = render(<Sidebar {...base} />);
    const time = container.querySelector("time");
    expect(time).toHaveTextContent("5分钟前");
    expect(time).toHaveAttribute("datetime", "2026-10-18T10:00:00Z");
    expect(time).toHaveAttribute("title", expect.stringContaining("最后更新："));
    act(() => vi.advanceTimersByTime(60_000));
    expect(time).toHaveTextContent("6分钟前");
    vi.setSystemTime(new Date("2026-10-18T11:05:00Z"));
    fireEvent.focus(window);
    expect(time).toHaveTextContent("1小时前");
  });

  it("不为缺失或损坏的日期伪造时间，未来日期显示刚刚", () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-10-18T10:05:00Z"));
    useSessionsStore.getState().setIndependent([
      { sessionId: "missing", title: "没有日期", cwd: "" },
      { sessionId: "invalid", title: "损坏日期", cwd: "", updatedAt: "invalid" },
      { sessionId: "future", title: "设备时钟偏差", cwd: "", updatedAt: "2026-10-18T10:06:00Z" },
    ]);
    const { container } = render(<Sidebar {...base} />);
    expect(container.querySelectorAll("time")).toHaveLength(1);
    expect(screen.getByText("刚刚")).toBeInTheDocument();
  });

  it("等待审批显示文字标记，其他活动状态保持简洁圆点", () => {
    const statuses: [SessionStatus, string][] = [
      ["working", "运行中"], ["planning", "规划中"], ["pending", "未开始"],
      ["awaiting_permission", "等待审批"], ["awaiting_answer", "等待回答"],
      ["awaiting_approval", "方案待批准"], ["pausing", "正在暂停"],
      ["paused", "已暂停"], ["stopping", "正在停止"], ["stopped", "已停止"], ["failed", "失败"],
    ];
    useSessionsStore.getState().setIndependent([
      ...statuses.map(([status]) => ({ sessionId: status, title: `会话 ${status}`, cwd: "", status })),
      { sessionId: "done", title: "普通历史", cwd: "", status: "completed" },
      { sessionId: "unknown", title: "状态未知", cwd: "" },
    ]);

    render(<Sidebar {...base} />);

    for (const [status, label] of statuses) {
      const button = screen.getByRole("button", { name: `会话 ${status} · ${label}` });
      const indicator = within(button).getByRole("img", { name: label });
      expect(indicator).toHaveAttribute("title", label);
      expect(indicator.textContent).toBe(status === "awaiting_permission" ? "等待审批" : "");
      expect(button).toHaveTextContent(`会话 ${status}`);
      if (status !== "awaiting_permission") expect(button).not.toHaveTextContent(label);
      expect(button).toHaveAttribute("title", `会话 ${status} · ${label}`);
    }
    for (const title of ["普通历史", "状态未知"]) {
      expect(within(screen.getByRole("button", { name: title })).queryByRole("img")).toBeNull();
    }
  });

  it("审批标记优先于时间显示，处理后恢复时间并保留选中与点击行为", () => {
    const onSelect = vi.fn();
    const session = { sessionId: "live", title: "后台分析", cwd: "/workspace", updatedAt: "2026-10-18T10:00:00Z" };
    useSessionsStore.getState().setIndependent([{ ...session, status: "working" }]);
    useSessionsStore.getState().setCurrent("live");
    render(<Sidebar {...base} onSelect={onSelect} />);
    const button = screen.getByRole("button", { name: /后台分析 · 运行中/ });
    fireEvent.click(within(button).getByRole("img", { name: "运行中" }));
    expect(onSelect).toHaveBeenCalledWith("live", "/workspace");

    for (const [status, label] of [["awaiting_permission", "等待审批"], ["failed", "失败"], ["completed", ""]] as const) {
      act(() => useSessionsStore.getState().upsert({ ...session, status }));
      expect(button).toHaveAttribute("aria-current", "page");
      if (status === "awaiting_permission") {
        expect(button.querySelector("time")).toBeNull();
        expect(within(button).getByRole("img", { name: "等待审批" })).toHaveAttribute("title", expect.stringContaining("等待审批 ·"));
      } else {
        expect(button.querySelector("time")).toHaveAttribute("datetime", useSessionsStore.getState().independent[0].updatedAt);
      }
      expect(button).toHaveAttribute("title", [session.title, label].filter(Boolean).join(" · "));
      if (label) expect(within(button).getByRole("img", { name: label })).toBeInTheDocument();
      else expect(within(button).queryByRole("img")).toBeNull();
      expect(button).not.toHaveTextContent("运行中");
    }
  });

  it("搜索按钮触发 onOpenSearch,设置按钮触发 onOpenSettings", () => {
    const onOpenSearch = vi.fn();
    const onOpenSettings = vi.fn();
    render(
      <Sidebar {...base} onOpenSearch={onOpenSearch} onOpenSettings={onOpenSettings} />
    );
    fireEvent.click(screen.getByRole("button", { name: "搜索全部任务" }));
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

  it("任务与非空项目折叠按钮暴露 aria-expanded 状态", () => {
    useProjectsStore.setState({projects:[{id:"fold",name:"客户项目",cwd:"/project",createdAt:"2026-10-25",connectors:[],experts:[],skills:[],plans:[],tasks:[],assets:[],members:[],conversations:[]} ]});
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
    useSessionsStore.setState({ deletedSessionIds: [],
      independent: [{ sessionId: "legacy", title: "升级前任务", cwd: "/Users/demo" }],
      workspaces: [{ cwd: "/Users/demo", sessionCount: 1 }],
      homeCwd: "/Users/demo/Documents/EchoAgent",
    });

    render(<Sidebar {...base} />);

    expect(screen.getByText("升级前任务")).toBeInTheDocument();
    expect(screen.queryByText("demo")).toBeNull();
    expect(screen.queryByRole("button", { name: "项目 (0)" })).toBeNull();
    expect(screen.getByRole("button", { name: "项目" })).toBeInTheDocument();
  });

  it("项目会话只在其真实项目下展示", () => {
    useSessionsStore.setState({ deletedSessionIds: [],
      independent: [{ sessionId: "project-session", title: "项目对话", cwd: "/workspace", status: "awaiting_answer" }],
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
    expect(screen.getByText("项目对话").closest("button")?.querySelector("time"))
      .toHaveAttribute("datetime", "2026-09-05T00:00:00.000Z");
    expect(screen.getByRole("button", { name: /项目对话 · 等待回答/ })).not.toHaveTextContent("等待回答");
    expect(screen.getByRole("img", { name: "等待回答" })).toHaveAttribute("title", "等待回答");
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
    await waitFor(() => expect(screen.getByText("需求探索").closest(".sidebar__conv")?.querySelector("button")).toHaveFocus());
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
    useSessionsStore.setState({ deletedSessionIds: [],
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

  it.each([false, true])("删除成功后由列表变化反馈，部分清理失败=%s 时才提醒", async (partial) => {
    vi.mocked(agentDeleteSession).mockResolvedValueOnce({ memorySummariesDeleted: 1, memoryCleanupWarning: partial ? "memory store busy" : undefined });
    useSessionsStore.getState().setIndependent([{ sessionId: "delete-feedback", title: "待删除任务", cwd: "/workspace" }]);
    const onToast = vi.fn();
    render(<Sidebar {...base} onToast={onToast} />);
    fireEvent.click(screen.getByRole("button", { name: "待删除任务的会话操作" }));
    fireEvent.click(screen.getByRole("menuitem", { name: "永久删除" }));
    expect(agentDeleteSession).not.toHaveBeenCalledWith("delete-feedback", "/workspace");
    fireEvent.click(screen.getByRole("button", { name: "永久删除" }));
    await waitFor(() => expect(screen.queryByText("待删除任务")).not.toBeInTheDocument());
    expect(useSessionsStore.getState().independent.some(s => s.sessionId === "delete-feedback")).toBe(false);
    if (partial) expect(onToast).toHaveBeenCalledWith(expect.stringContaining("对话已删除，部分记忆未能清理"));
    else expect(onToast).not.toHaveBeenCalled();
    vi.mocked(agentDeleteSession).mockClear();
  });

  it("删除失败保留任务并在确认框内说明，支持原位重试而不重复 Toast", async () => {
    vi.mocked(agentDeleteSession).mockRejectedValueOnce(new Error("EACCES: permission denied"));
    useSessionsStore.getState().setIndependent([{ sessionId: "delete-retry", title: "删除重试任务", cwd: "/workspace" }]);
    const onToast = vi.fn();
    render(<Sidebar {...base} onToast={onToast} />);
    fireEvent.click(screen.getByRole("button", { name: "删除重试任务的会话操作" }));
    fireEvent.click(screen.getByRole("menuitem", { name: "永久删除" }));
    fireEvent.click(screen.getByRole("button", { name: "永久删除" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("没有访问权限，请检查授权");
    expect(screen.getByText("删除重试任务")).toBeInTheDocument();
    expect(onToast).not.toHaveBeenCalled();
    expect(screen.getByRole("button", { name: "永久删除" })).toBeEnabled();
    fireEvent.click(screen.getByRole("button", { name: "永久删除" }));
    await waitFor(() => expect(screen.queryByText("删除重试任务")).not.toBeInTheDocument());
  });

  it("悬停「更多」自动展开，展示次级功能且不抢走焦点", async () => {
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
    expect(within(menu).getByRole("menuitem", { name: "自定义菜单栏" })).toBeInTheDocument();
    expect(within(menu).getAllByRole("menuitem")).toHaveLength(3);
    for (const settingsEntry of ["我的文件", "个人记忆", "用量统计", "通知渠道", "云存储", "已归档"]) {
      expect(within(menu).queryByText(settingsEntry)).not.toBeInTheDocument();
    }
    for (const removed of ["组织", "灵感", "网页预览", "策略设置", "发现"]) {
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
    const lastItem = within(menu).getByRole("menuitem", { name: "自定义菜单栏" });
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

  it.each(["专家·技能·连接器", "专家团", "技能", "连接器", "插件"])("扩展子页 %s 始终选中同一个侧栏入口", activeNav => {
    const onNavigate = vi.fn();
    render(<Sidebar {...base} activeNav={activeNav} onNavigate={onNavigate} />);
    expect(screen.getByRole("button", { name: "专家·技能·连接器" })).toHaveClass("sidebar__nav-item--active");
    expect(screen.getByText("更多").closest("button")).not.toHaveClass("sidebar__nav-item--active");
    fireEvent.click(screen.getByRole("button", { name: "专家·技能·连接器" }));
    expect(onNavigate).toHaveBeenCalledWith("专家·技能·连接器");
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

  it("主导航可直接进入代码开发工作台", () => {
    const onNavigate = vi.fn();
    render(<Sidebar {...base} onNavigate={onNavigate} />);

    fireEvent.click(screen.getByRole("button", {name:"代码开发"}));

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
