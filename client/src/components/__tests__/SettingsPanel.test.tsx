// @vitest-environment jsdom
import { describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import "@testing-library/jest-dom/vitest";

vi.mock("@/lib/agent-client", () => ({
  providersList: vi.fn().mockResolvedValue({ providers: [], models: [] }),
  agentsDefaultsGet: vi.fn().mockResolvedValue({
    defaultModel: "",
    defaultPermission: "",
    rememberToolApprovals: null,
  }),
  agentAuthStatus: vi.fn().mockResolvedValue({ ready: false, providers: [] }),
  commandsList: vi.fn().mockResolvedValue([]),
  exportTextFile: vi.fn().mockResolvedValue("/tmp/usage.csv"),
  echoAgentDataDir: vi.fn().mockResolvedValue("/tmp/.echo-agent"),
  desktopPreferencesGet: vi.fn().mockResolvedValue({ closeToTray: true, showNotificationTaskTitle: false }),
  desktopPreferencesSave: vi.fn(async (closeToTray: boolean) => ({ closeToTray, showNotificationTaskTitle: false })),
  desktopNotificationPreviewSave: vi.fn(async (showTaskTitle: boolean) => ({ closeToTray: true, showNotificationTaskTitle: showTaskTitle })),
  mcpList: vi.fn().mockResolvedValue([]),
  notificationList: vi.fn().mockResolvedValue([]),
  notificationMarkRead: vi.fn().mockResolvedValue(undefined),
  notificationMarkAllRead: vi.fn().mockResolvedValue(undefined),
  notificationClear: vi.fn().mockResolvedValue(undefined),
  permissionList: vi.fn().mockResolvedValue([]),
  permissionSave: vi.fn().mockResolvedValue(undefined),
  skillsList: vi.fn().mockResolvedValue([]),
  subagentsConfigGet: vi.fn().mockResolvedValue({ maxDepth: 1 }),
  subagentsConfigSave: vi.fn().mockResolvedValue(1),
  webSearchConfigGet: vi.fn().mockResolvedValue({ enabled: false, model: "" }),
  webSearchConfigSave: vi.fn().mockResolvedValue(undefined),
  memoryConfigGet: vi.fn().mockResolvedValue({
    enabled: true,
    initialInjectionEnabled: true,
    saveOnEnd: true,
    watcherEnabled: true,
    autoFlushEnabled: true,
    dreamEnabled: true,
  }),
  memoryConfigSave: vi.fn(async (memory) => memory),
  memoryFlush: vi.fn(),
  memoryDream: vi.fn(),
  memoryList: vi.fn().mockResolvedValue([]),
  openExternalUrl: vi.fn().mockResolvedValue(undefined),
}));

vi.mock("@/lib/org-client", () => ({
  orgSession: vi.fn().mockResolvedValue({ loggedIn: false }),
  orgSyncModelConfig: vi.fn(),
  listenOrgModelsChanged: vi.fn().mockResolvedValue(() => {}),
}));

vi.mock("../WeixinChannelPanel", () => ({
  WeixinChannelPanel: () => <div data-testid="weixin-channel-panel"><h3>微信远程对话</h3></div>,
}));

import { SettingsPanel } from "../SettingsPanel";
import { ThemeProvider } from "../ThemeProvider";
import {
  memoryConfigGet,
  memoryConfigSave,
  memoryList,
  notificationList,
  openExternalUrl,
  permissionList,
  permissionSave,
  subagentsConfigGet,
  subagentsConfigSave,
  webSearchConfigGet,
  webSearchConfigSave,
  desktopPreferencesSave,
  desktopPreferencesGet,
  desktopNotificationPreviewSave,
} from "@/lib/agent-client";
import { useSessionsStore } from "@/stores/sessions-store";
import { useProjectsStore } from "@/stores/projects-store";

function renderSettings() {
  return render(
    <ThemeProvider>
      <SettingsPanel open onClose={() => {}} />
    </ThemeProvider>,
  );
}

describe("SettingsPanel", () => {
  it("个人记忆在设置中使用当前工作区和会话上下文", async () => {
    vi.mocked(memoryList).mockClear();
    render(<ThemeProvider><SettingsPanel open initialSection="personal-memory" cwd="/workspace" sessionId="session-1" onClose={() => {}} /></ThemeProvider>);

    expect(await screen.findByRole("heading", { name: "个人记忆" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "记忆" })).toHaveAttribute("aria-current", "page");
    expect(screen.getByRole("button", { name: "个人记忆" })).toHaveAttribute("aria-pressed", "true");
    expect(screen.getByLabelText("当前记忆上下文")).toHaveTextContent("/workspace");
    expect(screen.getByLabelText("当前记忆上下文")).toHaveTextContent("已连接当前会话");
    await waitFor(() => expect(memoryList).toHaveBeenCalledWith("/workspace"));
  });

  it("打开后置焦点、圈定 Tab，Escape 关闭并恢复原焦点", async () => {
    const opener = document.createElement("button");
    opener.textContent = "打开设置";
    document.body.appendChild(opener);
    opener.focus();
    const onClose = vi.fn();
    const { rerender } = render(
      <ThemeProvider>
        <SettingsPanel open onClose={onClose} />
      </ThemeProvider>,
    );

    const close = screen.getByRole("button", { name: "关闭设置" });
    expect(close).toHaveFocus();
    const dialog = screen.getByRole("dialog", { name: "设置" });
    const focusable = Array.from(dialog.querySelectorAll<HTMLButtonElement>("button:not([disabled])"));
    focusable[focusable.length - 1]?.focus();
    fireEvent.keyDown(document, { key: "Tab" });
    expect(focusable[0]).toHaveFocus();

    fireEvent.keyDown(document, { key: "Escape" });
    expect(onClose).toHaveBeenCalledTimes(1);
    rerender(
      <ThemeProvider>
        <SettingsPanel open={false} onClose={onClose} />
      </ThemeProvider>,
    );
    expect(opener).toHaveFocus();
    opener.remove();
  });

  it("按使用场景分组全部设置入口，并默认打开模型页", async () => {
    const { container } = renderSettings();

    for (const group of ["消息与提醒", "智能体", "应用", "数据与支持"]) {
      expect(screen.getByRole("heading", { name: group, level: 2 })).toBeInTheDocument();
    }

    expect(container.querySelectorAll(".settings-navigation__item")).toHaveLength(13);
    expect(screen.getByRole("button", { name: "用量统计" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "通知" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "消息通道" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "云存储" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "模型" })).toHaveAttribute("aria-current", "page");
    expect(await screen.findByRole("heading", { name: "模型与连接", level: 2 })).toBeInTheDocument();
    expect(screen.getByText("内置对话模型开箱即用。默认顺序：手动指定、组织下发、个人连接、内置 chat-xc。"))
      .toBeInTheDocument();
  });

  it("允许 Slash 命令直接打开指定设置页", async () => {
    render(
      <ThemeProvider>
        <SettingsPanel open initialSection="help" onClose={() => {}} />
      </ThemeProvider>,
    );

    expect(screen.getByRole("button", { name: "帮助与更新" }))
      .toHaveAttribute("aria-current", "page");
    expect(await screen.findByRole("heading", { name: "帮助与更新", level: 2 }))
      .toBeInTheDocument();
  });

  it("通过桌面端系统浏览器打开帮助资源", async () => {
    render(
      <ThemeProvider>
        <SettingsPanel open initialSection="help" onClose={() => {}} />
      </ThemeProvider>,
    );

    fireEvent.click(await screen.findByRole("link", { name: /EchoAgent 文档/ }));

    await waitFor(() => {
      expect(openExternalUrl).toHaveBeenCalledWith("https://fuyuxiang.github.io/echo-agent/");
    });
  });

  it("所有设置入口都能进入对应页面并更新当前页状态", async () => {
    renderSettings();

    const pages = [
      "通知",
      "消息通道",
      "模型与连接",
      "智能体设置",
      "记忆",
      "系统设置",
      "个性化",
      "快捷键",
      "用量统计",
      "云存储",
      "数据管理",
      "安全中心",
      "帮助与更新",
    ];

    for (const page of pages) {
    const navigationItem = screen.getByRole("button", {
      name: page === "模型与连接" ? "模型" : page,
    });
    fireEvent.click(navigationItem);
      expect(await screen.findByRole("heading", {
        name: page === "通知" ? "通知中心" : page,
        level: 2,
      })).toBeInTheDocument();
      expect(navigationItem).toHaveAttribute("aria-current", "page");
    }
  }, 15_000);

  it("通知、记忆和数据在一个一级入口下保留完整子页面", async () => {
    renderSettings();
    fireEvent.click(screen.getByRole("button", { name: "通知" }));
    fireEvent.click(screen.getByRole("button", { name: "通知渠道" }));
    expect(await screen.findByRole("heading", { name: "通知渠道" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "通知" })).toHaveAttribute("aria-current", "page");
    expect(screen.queryByRole("button", { name: "微信远程对话" })).not.toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "记忆" }));
    fireEvent.click(screen.getByRole("button", { name: "个人记忆" }));
    expect(await screen.findByRole("heading", { name: "个人记忆" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "记忆" })).toHaveAttribute("aria-current", "page");

    fireEvent.click(screen.getByRole("button", { name: "数据管理" }));
    fireEvent.click(screen.getByRole("button", { name: "已归档" }));
    expect(await screen.findByRole("heading", { name: "已归档" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "数据管理" })).toHaveAttribute("aria-current", "page");
  });

  it("消息通道独立于通知视图，直接打开时也高亮正确入口", async () => {
    const { rerender } = render(
      <ThemeProvider><SettingsPanel open initialSection="weixin-channel" onClose={() => {}} /></ThemeProvider>,
    );
    expect(screen.getByRole("button", { name: "消息通道" })).toHaveAttribute("aria-current", "page");
    expect(screen.getByRole("button", { name: "通知" })).not.toHaveAttribute("aria-current");
    expect(screen.getByRole("heading", { name: "消息通道", level: 2 })).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "微信远程对话", level: 3 })).toBeInTheDocument();
    expect(screen.getByTestId("weixin-channel-panel")).toBeInTheDocument();
    expect(screen.queryByRole("group", { name: "通知视图" })).not.toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "通知" }));
    expect(await screen.findByRole("heading", { name: "通知中心" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "通知" })).toHaveAttribute("aria-current", "page");
    expect(screen.queryByTestId("weixin-channel-panel")).not.toBeInTheDocument();

    rerender(<ThemeProvider><SettingsPanel open={false} initialSection="weixin-channel" onClose={() => {}} /></ThemeProvider>);
    rerender(<ThemeProvider><SettingsPanel open initialSection="weixin-channel" onClose={() => {}} /></ThemeProvider>);
    expect(screen.getByRole("button", { name: "消息通道" })).toHaveAttribute("aria-current", "page");
  });

  it("安全中心说明网页与电脑操作按任务开启，不设置高风险全局默认值", async () => {
    render(
      <ThemeProvider>
        <SettingsPanel open initialSection="security" onClose={() => {}} />
      </ThemeProvider>,
    );

    expect(await screen.findByRole("heading", { name: "网页与电脑操作", level: 3 }))
      .toBeInTheDocument();
    expect(screen.getByText(/\+ 菜单为当前任务开启/)).toBeInTheDocument();
    expect(screen.queryByRole("checkbox", { name: /默认.*操作电脑/ })).toBeNull();
  });

  it("移除助理设置，并将智能体邮箱统一显示为事件收件箱", async () => {
    renderSettings();

    expect(screen.queryByRole("button", { name: "助理设置" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "账户管理" })).not.toBeInTheDocument();
    expect(screen.queryByText("智能体邮箱")).not.toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "通知" }));
    expect(await screen.findByRole("heading", { name: "通知中心" })).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "通知概览", level: 3 })).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "通知记录", level: 3 })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "全部" })).toHaveAttribute("aria-pressed", "true");
    expect(screen.getByRole("button", { name: "全部已读" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "清空" })).toBeDisabled();
  });

  it("通知类型可按后端 snake_case 值筛选", async () => {
    vi.mocked(notificationList).mockResolvedValueOnce([
      {
        id: 1,
        kind: "folder_trust",
        at: "2026-09-04T20:00:00+08:00",
        title: "需要信任工作区",
        severity: "warn",
        read: false,
      },
      {
        id: 2,
        kind: "info",
        at: "2026-09-04T20:01:00+08:00",
        title: "普通通知",
        severity: "info",
        read: true,
      },
    ]);
    render(
      <ThemeProvider>
        <SettingsPanel open initialSection="agent-mail" onClose={() => {}} />
      </ThemeProvider>,
    );

    expect(await screen.findByText("需要信任工作区")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "文件夹信任" }));
    expect(screen.getByText("需要信任工作区")).toBeInTheDocument();
    expect(screen.queryByText("普通通知")).not.toBeInTheDocument();
  });

  it("通知可直接打开关联会话以处理待办事件", async () => {
    vi.mocked(notificationList).mockResolvedValueOnce([{
      id: 9, kind: "permission", at: "2026-09-04T20:00:00+08:00",
      title: "等待授权", severity: "warn", read: false, sessionId: "session-123",
    }]);
    const onOpenSession = vi.fn().mockResolvedValue(undefined);
    const onClose = vi.fn();
    render(<ThemeProvider><SettingsPanel open initialSection="agent-mail" onOpenSession={onOpenSession} onClose={onClose} /></ThemeProvider>);
    fireEvent.click(await screen.findByRole("button", { name: /打开任务处理授权/ }));
    await waitFor(() => expect(onOpenSession).toHaveBeenCalledWith("session-123"));
    expect(onClose).toHaveBeenCalled();
  });

  it("桌面通知标题预览可关闭且不影响任务跳转入口", async () => {
    vi.mocked(notificationList).mockResolvedValueOnce([{
      id: 10, kind: "session_complete", at: "2026-09-04T20:00:00+08:00",
      title: "任务《整理报告》的回复已完成", severity: "info", read: false,
      sessionId: "session-456",
    }]);
    const onOpenSession = vi.fn().mockResolvedValue(undefined);
    render(<ThemeProvider><SettingsPanel open initialSection="agent-mail" onOpenSession={onOpenSession} onClose={() => {}} /></ThemeProvider>);
    const preview = await screen.findByRole("checkbox", { name: "桌面通知显示任务名称" });
    fireEvent.click(preview);
    await waitFor(() => expect(desktopNotificationPreviewSave).toHaveBeenCalledWith(true));
    fireEvent.click(screen.getByRole("button", { name: "打开相关任务" }));
    await waitFor(() => expect(onOpenSession).toHaveBeenCalledWith("session-456"));
  });

  it("无会话的自动化失败通知可打开关联运行记录", async () => {
    vi.mocked(notificationList).mockResolvedValueOnce([{
      id: 11, kind: "error", at: "2026-09-04T20:00:00+08:00",
      title: "自动化《日报》运行失败", severity: "error", read: false,
      automationId: "automation-1",
    }]);
    const onOpenAutomation = vi.fn().mockResolvedValue(undefined);
    render(<ThemeProvider><SettingsPanel open initialSection="agent-mail" onOpenAutomation={onOpenAutomation} onClose={() => {}} /></ThemeProvider>);
    fireEvent.click(await screen.findByRole("button", { name: "打开自动化记录" }));
    await waitFor(() => expect(onOpenAutomation).toHaveBeenCalledWith("automation-1"));
  });

  it("关联会话打开失败时保留设置页并显示准确错误", async () => {
    vi.mocked(notificationList).mockResolvedValueOnce([{
      id: 9, kind: "permission", at: "2026-09-04T20:00:00+08:00",
      title: "等待授权", severity: "warn", read: false, sessionId: "session-123",
    }]);
    const onClose = vi.fn();
    render(<ThemeProvider><SettingsPanel open initialSection="agent-mail"
      onOpenSession={vi.fn().mockRejectedValue(new Error("会话已删除"))} onClose={onClose} /></ThemeProvider>);
    fireEvent.click(await screen.findByRole("button", { name: /打开任务处理授权/ }));
    expect(await screen.findByRole("alert")).toHaveTextContent("打开关联任务失败：会话已删除");
    expect(onClose).not.toHaveBeenCalled();
  });

  it("通知文件损坏时显示可恢复错误而不是伪装成空列表", async () => {
    vi.mocked(notificationList).mockRejectedValueOnce(new Error("解析通知记录失败"));
    render(
      <ThemeProvider>
        <SettingsPanel open initialSection="agent-mail" onClose={() => {}} />
      </ThemeProvider>,
    );

    expect(await screen.findByRole("alert")).toHaveTextContent(
      "通知记录不可用：解析通知记录失败。原文件未被覆盖；你可以修复文件后重试，或点击“清空”重建。",
    );
    expect(screen.getByRole("button", { name: "清空" })).toBeEnabled();
  });

  it("记忆读取失败禁止默认值写回，重试后使用 revision 保存单字段", async () => {
    vi.mocked(memoryConfigGet).mockRejectedValueOnce(new Error("read failed"));
    vi.mocked(memoryConfigSave).mockClear();
    render(<ThemeProvider><SettingsPanel open initialSection="memory" onClose={() => {}} /></ThemeProvider>);
    await screen.findByText(/配置未加载/);
    for (const input of screen.getAllByRole("checkbox")) expect(input).toBeDisabled();
    fireEvent.click(screen.getByRole("checkbox", { name: "自动整理" }));
    expect(memoryConfigSave).not.toHaveBeenCalled();
    vi.mocked(memoryConfigGet).mockResolvedValueOnce({ enabled: true, initialInjectionEnabled: false, saveOnEnd: false, watcherEnabled: false, autoFlushEnabled: false, dreamEnabled: false, revision: "r1" });
    fireEvent.click(screen.getByRole("button", { name: "重新加载" }));
    await waitFor(() => expect(screen.getByRole("checkbox", { name: "自动整理" })).toBeEnabled());
    fireEvent.click(screen.getByRole("checkbox", { name: "自动整理" }));
    await waitFor(() => expect(memoryConfigSave).toHaveBeenCalledWith({ dreamEnabled: true }, "r1"));
    expect(screen.getByRole("checkbox", { name: "监听外部修改" })).not.toBeChecked();
  });

  it("记忆开关仅保存更改字段", async () => {
    render(
      <ThemeProvider>
        <SettingsPanel open initialSection="memory" onClose={() => {}} />
      </ThemeProvider>,
    );

    const autoFlush = await screen.findByRole("checkbox", { name: "自动提取" });
    expect(screen.queryByLabelText("记忆检索方式")).not.toBeInTheDocument();
    expect(autoFlush).toBeChecked();
    fireEvent.click(autoFlush);

    await waitFor(() => {
      expect(memoryConfigSave).toHaveBeenCalledWith({ autoFlushEnabled: false }, undefined);
    });
    expect(
      screen.getByText("记忆配置已保存，重启 Agent 后对新会话生效。"),
    ).toBeInTheDocument();
  });

  it("默认关闭到托盘，并可在系统设置中关闭", async () => {
    render(
      <ThemeProvider>
        <SettingsPanel open initialSection="general" onClose={() => {}} />
      </ThemeProvider>,
    );

    const toggle = await screen.findByRole("checkbox", { name: "关闭窗口时继续后台运行" });
    await waitFor(() => expect(toggle).toBeEnabled());
    expect(toggle).toBeChecked();

    fireEvent.click(toggle);

    await waitFor(() => {
      expect(desktopPreferencesSave).toHaveBeenCalledWith(false);
      expect(toggle).not.toBeChecked();
    });
    expect(screen.getByRole("status")).toHaveTextContent("关闭主窗口将停止当前 Runtime 和自动化调度");
    expect(screen.getByText("已关闭后台运行，下次关闭窗口将退出 EchoAgent。")).toBeInTheDocument();
  });

  it("桌面偏好保存失败时回滚后台运行开关", async () => {
    vi.mocked(desktopPreferencesSave).mockRejectedValueOnce(new Error("偏好文件只读"));
    render(
      <ThemeProvider>
        <SettingsPanel open initialSection="general" onClose={() => {}} />
      </ThemeProvider>,
    );

    const toggle = await screen.findByRole("checkbox", { name: "关闭窗口时继续后台运行" });
    await waitFor(() => expect(toggle).toBeEnabled());
    fireEvent.click(toggle);

    await waitFor(() => expect(toggle).toBeChecked());
    expect(screen.getByText("保存桌面偏好失败：偏好文件只读")).toBeInTheDocument();
  });

  it("桌面偏好读取失败时不允许覆盖未知配置，重试后恢复操作", async () => {
    vi.mocked(desktopPreferencesGet).mockRejectedValueOnce(new Error("偏好文件不可读"));
    render(<ThemeProvider><SettingsPanel open initialSection="general" onClose={() => {}} /></ThemeProvider>);
    const toggle = await screen.findByRole("checkbox", { name: "关闭窗口时继续后台运行" });
    expect(await screen.findByRole("alert")).toHaveTextContent("偏好文件不可读");
    expect(toggle).toBeDisabled();
    fireEvent.click(screen.getByRole("button", { name: "重试" }));
    await waitFor(() => expect(toggle).toBeEnabled());
  });

  it("运行时配置分项读取失败时不使用默认值且禁止保存", async () => {
    vi.mocked(subagentsConfigGet).mockRejectedValueOnce(new Error("子代理配置损坏"));
    const { container } = render(
      <ThemeProvider>
        <SettingsPanel open initialSection="agent-settings" onClose={() => {}} />
      </ThemeProvider>,
    );

    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent("子代理配置读取失败：子代理配置损坏");
    const depth = container.querySelector<HTMLInputElement>('input[type="number"]');
    expect(depth).toBeDisabled();
    expect(depth).toHaveValue(null);
    expect(subagentsConfigSave).not.toHaveBeenCalled();

    fireEvent.click(screen.getByRole("button", { name: "重试" }));
    await waitFor(() => expect(screen.queryByRole("alert")).not.toBeInTheDocument());
    expect(depth).toBeEnabled();
    expect(depth).toHaveValue(1);
  });

  it("天气和搜索作为默认能力，无需在设置里指定搜索模型", async () => {
    render(<ThemeProvider><SettingsPanel open initialSection="agent-settings" onClose={() => {}} /></ThemeProvider>);
    expect(await screen.findByText("子代理嵌套深度")).toBeInTheDocument();
    expect(screen.queryByRole("textbox", { name: "Web 搜索模型 ID" })).not.toBeInTheDocument();
    expect(webSearchConfigGet).not.toHaveBeenCalled();
    expect(webSearchConfigSave).not.toHaveBeenCalled();
  });

  it("权限规则加载失败时不伪装空列表或允许覆盖，重试后恢复", async () => {
    vi.mocked(permissionList)
      .mockReset()
      .mockRejectedValueOnce(new Error("权限配置损坏"))
      .mockResolvedValueOnce([{ action: "deny", tool: "bash", pattern: "rm *" }]);
    vi.mocked(permissionSave).mockClear();
    render(
      <ThemeProvider>
        <SettingsPanel open initialSection="security" onClose={() => {}} />
      </ThemeProvider>,
    );

    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent("权限规则读取失败：权限配置损坏");
    expect(screen.queryByText("尚未添加自定义权限规则")).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "保存全部规则" })).toBeDisabled();
    expect(permissionSave).not.toHaveBeenCalled();

    fireEvent.click(screen.getByRole("button", { name: "重试" }));
    await waitFor(() => expect(screen.queryByRole("alert")).not.toBeInTheDocument());
    expect(screen.getByText("rm *")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "保存全部规则" })).toBeEnabled();
  });

  it("在设置中集中查看、恢复并打开归档会话", async () => {
    useSessionsStore.setState({
      independent: [
        { sessionId: "active", title: "活动任务", cwd: "/home", archived: false },
        { sessionId: "archived", title: "发布复盘", cwd: "/workspace", archived: true, updatedAt: "2026-09-14T08:00:00Z" },
      ],
    });
    useProjectsStore.setState({
      projects: [{
        id: "p1",
        name: "发布项目",
        cwd: "/workspace",
        createdAt: "2026-09-14T07:00:00Z",
        connectors: [], experts: [], skills: [], plans: [], tasks: [], assets: [], members: [],
        conversations: [{
          sessionId: "archived",
          title: "发布复盘",
          createdAt: "2026-09-14T08:00:00Z",
          archived: true,
        }],
      }],
    });
    const onRestoreSession = vi.fn().mockResolvedValue(undefined);
    const onOpenSession = vi.fn().mockResolvedValue(undefined);
    const onClose = vi.fn();
    render(
      <ThemeProvider>
        <SettingsPanel
          open
          initialSection="archived"
          onClose={onClose}
          onRestoreSession={onRestoreSession}
          onOpenSession={onOpenSession}
        />
      </ThemeProvider>,
    );

    expect(screen.getByRole("heading", { name: "已归档", level: 2 })).toBeInTheDocument();
    expect(screen.getByText("发布复盘")).toBeInTheDocument();
    expect(screen.getByText(/发布项目 · 项目对话/)).toBeInTheDocument();
    expect(screen.queryByText("活动任务")).toBeNull();
    expect(screen.getByLabelText("1 个已归档会话")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "恢复并打开" }));
    await waitFor(() => expect(onRestoreSession).toHaveBeenCalledWith("archived", false, "/workspace"));
    expect(onClose).toHaveBeenCalledTimes(1);
    expect(onOpenSession).toHaveBeenCalledWith("archived", "/workspace");
  });

  it("归档管理支持搜索、范围筛选与永久删除确认", async () => {
    useSessionsStore.setState({
      independent: [
        { sessionId: "standalone", title: "独立归档", cwd: "/home", archived: true },
        { sessionId: "project-chat", title: "项目归档", cwd: "/workspace", archived: true },
      ],
    });
    useProjectsStore.setState({
      projects: [{
        id: "p1", name: "交付项目", cwd: "/workspace", createdAt: "2026-09-14T07:00:00Z",
        connectors: [], experts: [], skills: [], plans: [], tasks: [], assets: [], members: [],
        conversations: [{ sessionId: "project-chat", title: "项目归档", createdAt: "2026-09-14T08:00:00Z", archived: true }],
      }],
    });
    const onDeleteSession = vi.fn().mockResolvedValue(undefined);
    render(
      <ThemeProvider>
        <SettingsPanel
          open
          initialSection="archived"
          onClose={() => {}}
          onRestoreSession={vi.fn().mockResolvedValue(undefined)}
          onOpenSession={vi.fn()}
          onDeleteSession={onDeleteSession}
        />
      </ThemeProvider>,
    );

    fireEvent.click(screen.getByRole("button", { name: "项目 1" }));
    expect(screen.getByText("项目归档")).toBeInTheDocument();
    expect(screen.queryByText("独立归档")).toBeNull();

    fireEvent.change(screen.getByRole("searchbox", { name: "搜索已归档会话" }), { target: { value: "不存在" } });
    expect(screen.getByText("没有匹配的归档会话")).toBeInTheDocument();
    fireEvent.change(screen.getByRole("searchbox", { name: "搜索已归档会话" }), { target: { value: "项目" } });

    fireEvent.click(screen.getByRole("button", { name: "永久删除 项目归档" }));
    expect(screen.getByRole("alertdialog", { name: "永久删除对话“项目归档”？" })).toHaveTextContent("无法恢复");
    fireEvent.click(screen.getByRole("button", { name: "永久删除" }));
    await waitFor(() => expect(onDeleteSession).toHaveBeenCalledWith("project-chat", "/workspace"));
  });

  it("归档管理通过显式选择安全地批量永久删除当前筛选结果", async () => {
    useSessionsStore.setState({
      independent: [
        { sessionId: "standalone", title: "独立归档", cwd: "/home", archived: true },
        { sessionId: "project-chat", title: "项目归档", cwd: "/workspace", archived: true },
        { sessionId: "active", title: "活动任务", cwd: "/home", archived: false },
      ],
    });
    useProjectsStore.setState({
      projects: [{
        id: "p1", name: "交付项目", cwd: "/workspace", createdAt: "2026-09-14T07:00:00Z",
        connectors: [], experts: [], skills: [], plans: [], tasks: [], assets: [], members: [],
        conversations: [{ sessionId: "project-chat", title: "项目归档", createdAt: "2026-09-14T08:00:00Z", archived: true }],
      }],
    });
    const onDeleteSession = vi.fn().mockResolvedValue(undefined);
    render(
      <ThemeProvider>
        <SettingsPanel
          open
          initialSection="archived"
          onClose={() => {}}
          onRestoreSession={vi.fn().mockResolvedValue(undefined)}
          onOpenSession={vi.fn()}
          onDeleteSession={onDeleteSession}
        />
      </ThemeProvider>,
    );

    expect(screen.queryByRole("button", { name: /恢复当前/ })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "批量管理" }));
    fireEvent.click(screen.getByRole("button", { name: "选择当前 2 项" }));
    expect(screen.getByText("已选 2 项")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "项目 1" }));
    expect(screen.getByText("已选 0 项")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "永久删除" })).toBeDisabled();

    fireEvent.click(screen.getByRole("button", { name: "选择当前 1 项" }));
    expect(screen.getByRole("checkbox", { name: "选择 项目归档" })).toBeChecked();
    fireEvent.click(screen.getByRole("button", { name: "永久删除" }));

    expect(screen.getByRole("alertdialog", { name: "永久删除所选 1 个对话？" }))
      .toHaveTextContent("无法恢复");
    expect(onDeleteSession).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "永久删除 1 个对话" }));

    await waitFor(() => expect(onDeleteSession).toHaveBeenCalledTimes(1));
    expect(onDeleteSession).toHaveBeenCalledWith("project-chat", "/workspace");
    expect(screen.getByRole("button", { name: "批量管理" })).toBeInTheDocument();
  });

  it("批量恢复发生部分失败时只保留失败项并提供可重试反馈", async () => {
    useSessionsStore.setState({
      independent: [
        { sessionId: "restore-ok", title: "可恢复归档", cwd: "/home", archived: true },
        { sessionId: "restore-failed", title: "恢复失败归档", cwd: "/workspace", archived: true },
      ],
    });
    useProjectsStore.setState({ projects: [] });
    const onRestoreSession = vi.fn(async (sessionId: string) => {
      if (sessionId === "restore-failed") throw new Error("磁盘不可写");
    });
    const onToast = vi.fn();
    render(
      <ThemeProvider>
        <SettingsPanel
          open
          initialSection="archived"
          onClose={() => {}}
          onRestoreSession={onRestoreSession}
          onOpenSession={vi.fn()}
          onDeleteSession={vi.fn().mockResolvedValue(undefined)}
          onToast={onToast}
        />
      </ThemeProvider>,
    );

    fireEvent.click(screen.getByRole("button", { name: "批量管理" }));
    fireEvent.click(screen.getByRole("button", { name: "选择当前 2 项" }));
    fireEvent.click(screen.getByRole("button", { name: "恢复所选" }));
    expect(screen.getByRole("dialog", { name: "恢复所选 2 个归档会话？" }))
      .toHaveTextContent("其他归档会话保持不变");
    fireEvent.click(screen.getByRole("button", { name: "恢复 2 个会话" }));

    await waitFor(() => expect(onRestoreSession).toHaveBeenCalledTimes(2));
    expect(await screen.findByRole("alert")).toHaveTextContent("已恢复 1 个，1 个恢复失败");
    expect(screen.getByRole("checkbox", { name: "选择 可恢复归档" })).not.toBeChecked();
    expect(screen.getByRole("checkbox", { name: "选择 恢复失败归档" })).toBeChecked();
    expect(onToast).toHaveBeenCalledWith("已恢复 1 个，1 个恢复失败");
  });
});
