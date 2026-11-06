import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

vi.mock("@/lib/agent-client", () => ({
  agentsList: vi.fn().mockResolvedValue([]),
  skillsList: vi.fn().mockResolvedValue([]),
  mcpList: vi.fn().mockResolvedValue([
    { name: "真实服务", target: "https://mcp.example.com", enabled: true },
    { name: "已停用服务", target: "https://disabled.example.com", enabled: false },
  ]),
}));

import { InputAddMenu } from "../InputAddMenu";
import { agentsList } from "@/lib/agent-client";

describe("InputAddMenu", () => {
  it("按历史页面入口可用状态显示会议录音与纪要", async () => {
    const onOpenMeetingMinutes = vi.fn();
    const { rerender } = render(
      <InputAddMenu
        onPickFiles={vi.fn()}
        meetingMinutesAvailable={false}
        onOpenMeetingMinutes={onOpenMeetingMinutes}
      />,
    );
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "添加" }));
    });
    expect(screen.queryByRole("menuitem", { name: /会议录音与纪要/ })).toBeNull();

    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "添加" }));
      rerender(
        <InputAddMenu
          onPickFiles={vi.fn()}
          meetingMinutesAvailable
          onOpenMeetingMinutes={onOpenMeetingMinutes}
        />,
      );
      fireEvent.click(screen.getByRole("button", { name: "添加" }));
    });
    await act(async () => {
      fireEvent.click(screen.getByRole("menuitem", { name: /会议录音与纪要/ }));
    });
    expect(onOpenMeetingMinutes).toHaveBeenCalledTimes(1);
  });

  it("未提供任务能力回调时不展示操作入口", async () => {
    render(<InputAddMenu onPickFiles={vi.fn()} />);

    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "添加" }));
    });
    expect(screen.queryByText("操作网页")).toBeNull();
    expect(screen.queryByText("操作电脑")).toBeNull();
  });

  it("鼠标打开菜单时保留触发器焦点，不强制高亮添加文件", async () => {
    render(<InputAddMenu onPickFiles={vi.fn()} />);

    const trigger = screen.getByRole("button", { name: "添加" });
    trigger.focus();
    await act(async () => {
      fireEvent.click(trigger, { detail: 1 });
    });

    expect(trigger).toHaveFocus();
    expect(screen.getByRole("menuitem", { name: "添加文件" })).not.toHaveFocus();
  });

  it.each([false, true])("Tab 离开菜单时关闭弹层（Shift=%s）", async (shiftKey) => {
    render(<InputAddMenu onPickFiles={vi.fn()} />);
    fireEvent.click(screen.getByRole("button", { name: "添加" }));
    fireEvent.keyDown(screen.getByRole("menuitem", { name: "添加文件" }), { key: "Tab", shiftKey });
    expect(screen.queryByRole("menu")).toBeNull();
    expect(screen.getByRole("button", { name: "添加" })).toHaveAttribute("aria-expanded", "false");
  });

  it("在 + 菜单中用中文能力名按任务开启，不暴露内部模式名", async () => {
    const onAutomationModeChange = vi.fn();
    render(
      <InputAddMenu
        onPickFiles={vi.fn()}
        automationMode="default"
        automationCapabilities={{
          browser: { available: true, browserName: "Chrome" },
          computer: {
            available: false,
            platform: "macOS",
            screenCapture: false,
            inputControl: false,
            reason: "需要系统权限",
          },
        }}
        onAutomationModeChange={onAutomationModeChange}
      />,
    );

    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "添加" }));
    });
    fireEvent.click(screen.getByRole("menuitemradio", { name: /操作网页/ }));
    expect(onAutomationModeChange).toHaveBeenCalledWith("browser_use");
    expect(screen.queryByText("Browser Use")).toBeNull();
    expect(screen.queryByText("Computer Use")).toBeNull();

    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "添加" }));
    });
    expect(screen.getByRole("menuitemradio", { name: /操作电脑/ }))
      .toHaveAttribute("aria-disabled", "true");
  });

  it("连接器子菜单只展示后端实际启用的 MCP 服务", async () => {
    const onNavigateConnectors = vi.fn();
    render(
      <InputAddMenu
        onPickFiles={vi.fn()}
        onNavigateConnectors={onNavigateConnectors}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "添加" }));
    const connectorRow = screen.getByText("连接器").closest(".iam-item");
    expect(connectorRow).not.toBeNull();
    fireEvent.mouseEnter(connectorRow!);

    const realServer = await screen.findByText("真实服务");
    expect(screen.queryByText("已停用服务")).toBeNull();
    fireEvent.click(realServer);
    await waitFor(() => expect(onNavigateConnectors).toHaveBeenCalledTimes(1));
  });

  it("显示会议录音与纪要时，三个目录子菜单仍跟随各自的实际行位置", async () => {
    render(
      <InputAddMenu
        onPickFiles={vi.fn()}
        meetingMinutesAvailable
        onOpenMeetingMinutes={vi.fn()}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "添加" }), { detail: 1 });
    const rows = [
      { label: "专家", offsetTop: 176 },
      { label: "技能", offsetTop: 228 },
      { label: "连接器", offsetTop: 280 },
    ];

    for (const { label, offsetTop } of rows) {
      const row = screen.getByRole("menuitem", { name: label });
      Object.defineProperty(row, "offsetTop", { configurable: true, value: offsetTop });
      fireEvent.mouseEnter(row);

      const submenu = await screen.findByRole("menu", { name: `${label}子菜单` });
      await waitFor(() => {
        expect(submenu.style.getPropertyValue("--iam-submenu-top")).toBe(`${offsetTop}px`);
      });
    }
  });

  it("空间不足时上移二级菜单并自动翻到主菜单左侧", async () => {
    render(<InputAddMenu onPickFiles={vi.fn()} />);

    fireEvent.click(screen.getByRole("button", { name: "添加" }), { detail: 1 });
    const connectorRow = screen.getByRole("menuitem", { name: "连接器" });
    Object.defineProperty(connectorRow, "offsetTop", { configurable: true, value: 600 });
    fireEvent.mouseEnter(connectorRow);

    const submenu = await screen.findByRole("menu", { name: "连接器子菜单" });
    const popover = screen.getByRole("menu", { name: "添加内容" });
    Object.defineProperty(submenu, "offsetHeight", { configurable: true, value: 300 });
    Object.defineProperty(submenu, "offsetWidth", { configurable: true, value: 280 });
    const popoverTop = 100;
    const popoverRight = window.innerWidth - 20;
    const popoverLeft = popoverRight - 280;
    const rectSpy = vi.spyOn(popover, "getBoundingClientRect").mockReturnValue({
      x: popoverLeft,
      y: popoverTop,
      top: popoverTop,
      right: popoverRight,
      bottom: popoverTop + 400,
      left: popoverLeft,
      width: 280,
      height: 400,
      toJSON: () => ({}),
    });

    try {
      fireEvent(window, new Event("resize"));
      const expectedTop = window.innerHeight - 8 - popoverTop - 300;
      await waitFor(() => {
        expect(submenu.style.getPropertyValue("--iam-submenu-top")).toBe(`${expectedTop}px`);
        expect(submenu).toHaveClass("iam-submenu--left");
      });
    } finally {
      rectSpy.mockRestore();
    }
  });

  it("支持方向键进入子菜单，Escape 关闭并恢复触发器焦点", async () => {
    vi.mocked(agentsList).mockResolvedValueOnce([
      {
        name: "代码专家",
        path: "/agents/code.md",
        description: "审查代码",
        scope: "user",
        raw: "# 代码专家",
      },
    ]);
    render(<InputAddMenu onPickFiles={vi.fn()} onSelectExpert={vi.fn()} />);

    const trigger = screen.getByRole("button", { name: "添加" });
    fireEvent.click(trigger, { detail: 0 });
    expect(trigger).toHaveAttribute("aria-expanded", "true");
    expect(screen.getByRole("menu", { name: "添加内容" })).toBeInTheDocument();

    const addFiles = screen.getByRole("menuitem", { name: "添加文件" });
    expect(addFiles).toHaveFocus();
    fireEvent.keyDown(addFiles, { key: "ArrowDown" });
    const experts = screen.getByRole("menuitem", { name: "专家" });
    expect(experts).toHaveFocus();
    fireEvent.keyDown(experts, { key: "ArrowRight" });

    expect(await screen.findByRole("menu", { name: "专家子菜单" })).toBeInTheDocument();
    await waitFor(() => expect(screen.getByRole("menuitem", { name: /代码专家/ })).toHaveFocus());

    fireEvent.keyDown(document, { key: "Escape" });
    expect(trigger).toHaveAttribute("aria-expanded", "false");
    expect(trigger).toHaveFocus();
  });

  it("加载失败时不误报为空列表，并可就地重试", async () => {
    vi.mocked(agentsList)
      .mockRejectedValueOnce(new Error("offline"))
      .mockResolvedValueOnce([
        {
          name: "恢复后的专家",
          path: "/agents/recovered.md",
          description: "可用",
          scope: "user",
          raw: "# 恢复后的专家",
        },
      ]);
    render(<InputAddMenu onPickFiles={vi.fn()} onSelectExpert={vi.fn()} />);

    fireEvent.click(screen.getByRole("button", { name: "添加" }));
    fireEvent.mouseEnter(screen.getByRole("menuitem", { name: "专家" }));

    expect(await screen.findByRole("alert")).toHaveTextContent("专家加载失败");
    expect(screen.queryByText("暂无已安装专家")).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("menuitem", { name: "重试" }));

    expect(await screen.findByRole("menuitem", { name: /恢复后的专家/ })).toBeInTheDocument();
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
    await waitFor(() => expect(screen.getByRole("menuitem", { name: /恢复后的专家/ })).toHaveFocus());
  });

  it("窄窗口目录需要点击进入，鼠标离开不会丢失页面，返回恢复父项焦点", async () => {
    const onSelectExpert = vi.fn();
    vi.mocked(agentsList).mockResolvedValueOnce([
      { name: "代码专家", path: "/agents/code.md", description: "审查代码", scope: "user", raw: "检查代码" },
    ]);
    render(<InputAddMenu onPickFiles={vi.fn()} onSelectExpert={onSelectExpert} />);
    fireEvent.click(screen.getByRole("button", { name: "添加" }), { detail: 1 });
    const popover = screen.getByRole("menu", { name: "添加内容" });
    const rectSpy = vi.spyOn(popover, "getBoundingClientRect").mockReturnValue({
      x: 30, y: 400, left: 30, top: 400, right: 254, bottom: 640,
      width: 224, height: 240, toJSON: () => ({}),
    });
    const originalWidth = window.innerWidth;
    try {
      Object.defineProperty(window, "innerWidth", { configurable: true, value: 350 });
      fireEvent(window, new Event("resize"));
      const experts = screen.getByRole("menuitem", { name: "专家" });
      fireEvent.mouseEnter(experts);
      await act(() => new Promise(resolve => setTimeout(resolve, 200)));
      expect(screen.queryByRole("menu", { name: "专家子菜单" })).toBeNull();
      fireEvent.click(experts, { detail: 1 });
      const catalog = await screen.findByRole("menu", { name: "专家子菜单" });
      expect(catalog).toHaveClass("iam-submenu--panel");
      expect(screen.queryByRole("menuitem", { name: "技能" })).toBeNull();
      fireEvent.mouseLeave(catalog);
      await act(() => new Promise(resolve => setTimeout(resolve, 250)));
      expect(catalog).toBeVisible();
      fireEvent.click(screen.getByRole("menuitem", { name: "返回添加菜单" }));
      expect(experts).toHaveFocus();
      expect(screen.getByRole("menuitem", { name: "技能" })).toBeVisible();
      expect(screen.queryByRole("menu", { name: "专家子菜单" })).toBeNull();
      expect(onSelectExpert).not.toHaveBeenCalled();
      fireEvent.click(screen.getByRole("menuitem", { name: "技能" }), { detail: 1 });
      const skills = await screen.findByRole("menu", { name: "技能子菜单" });
      fireEvent.keyDown(skills, { key: "ArrowLeft" });
      expect(screen.getByRole("menuitem", { name: "技能" })).toHaveFocus();
    } finally {
      rectSpy.mockRestore();
      Object.defineProperty(window, "innerWidth", { configurable: true, value: originalWidth });
    }
  });

  it("空目录提供可聚焦的返回入口，Escape 仍关闭菜单并恢复添加按钮焦点", async () => {
    render(<InputAddMenu onPickFiles={vi.fn()} />);
    const trigger = screen.getByRole("button", { name: "添加" });
    fireEvent.click(trigger, { detail: 0 });
    const popover = screen.getByRole("menu", { name: "添加内容" });
    const rectSpy = vi.spyOn(popover, "getBoundingClientRect").mockReturnValue({
      x: 30, y: 400, left: 30, top: 400, right: 254, bottom: 640,
      width: 224, height: 240, toJSON: () => ({}),
    });
    const originalWidth = window.innerWidth;
    try {
      Object.defineProperty(window, "innerWidth", { configurable: true, value: 350 });
      fireEvent(window, new Event("resize"));
      fireEvent.keyDown(screen.getByRole("menuitem", { name: "专家" }), { key: "ArrowRight" });
      expect(await screen.findByText("暂无已安装专家")).toBeVisible();
      await waitFor(() => expect(screen.getByRole("menuitem", { name: "返回添加菜单" })).toHaveFocus());
      fireEvent.keyDown(document, { key: "Escape" });
      expect(screen.queryByRole("menu")).toBeNull();
      expect(trigger).toHaveFocus();
    } finally {
      rectSpy.mockRestore();
      Object.defineProperty(window, "innerWidth", { configurable: true, value: originalWidth });
    }
  });

  it("打开目录时调整窗口保持所选目录和焦点，返回按钮消失后焦点回到条目", async () => {
    render(<InputAddMenu onPickFiles={vi.fn()} />);
    fireEvent.click(screen.getByRole("button", { name: "添加" }));
    const popover = screen.getByRole("menu", { name: "添加内容" });
    const rectSpy = vi.spyOn(popover, "getBoundingClientRect").mockReturnValue({
      x: 30, y: 400, left: 30, top: 400, right: 254, bottom: 640,
      width: 224, height: 240, toJSON: () => ({}),
    });
    const originalWidth = window.innerWidth;
    try {
      const connectors = screen.getByRole("menuitem", { name: "连接器" });
      connectors.focus();
      fireEvent.keyDown(connectors, { key: "ArrowRight" });
      const catalog = await screen.findByRole("menu", { name: "连接器子菜单" });
      const server = await screen.findByRole("menuitem", { name: /真实服务/ });
      await waitFor(() => expect(server).toHaveFocus());
      connectors.focus();
      Object.defineProperty(window, "innerWidth", { configurable: true, value: 350 });
      fireEvent(window, new Event("resize"));
      expect(catalog).toHaveClass("iam-submenu--panel");
      await waitFor(() => expect(server).toHaveFocus());
      screen.getByRole("menuitem", { name: "返回添加菜单" }).focus();
      Object.defineProperty(window, "innerWidth", { configurable: true, value: originalWidth });
      fireEvent(window, new Event("resize"));
      expect(catalog).not.toHaveClass("iam-submenu--panel");
      await waitFor(() => expect(server).toHaveFocus());
    } finally {
      rectSpy.mockRestore();
      Object.defineProperty(window, "innerWidth", { configurable: true, value: originalWidth });
    }
  });
});
