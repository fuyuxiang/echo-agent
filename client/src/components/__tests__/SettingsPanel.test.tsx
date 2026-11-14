// @vitest-environment jsdom
import { beforeEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
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
  internalReload: vi.fn().mockResolvedValue(undefined),
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
  memoryRecordsGet: vi.fn().mockResolvedValue({ enabled: true, records: [], pendingCount: 0, extraction: {outcome:"",detail:"",recordsWritten:0}, retrieval:{at:0,sources:[],count:0,mode:"",durationMs:0} }),
  memoryRecordSave: vi.fn(), memoryRecordRetire: vi.fn(),
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

vi.mock("@/lib/notify-channels", async (original) => ({
  ...await original<object>(),
  loadNotifyChannels: vi.fn(async () => []),
  loadDesktopNotifyStatus: vi.fn(async () => ({enabled:true,source:"default",permission:"unknown"})),
  testDesktopNotify: vi.fn(async () => ({id:"desktop",ok:true})),
}));
import { testDesktopNotify } from "@/lib/notify-channels";
import { SettingsPanel } from "../SettingsPanel";
import { ThemeProvider } from "../ThemeProvider";
import {
  providersList,
  memoryConfigGet,
  memoryConfigSave,
  memoryList,
  notificationList,
  openExternalUrl,
  permissionList,
  permissionSave,
  internalReload,
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

beforeEach(() => vi.clearAllMocks());

describe("Settings navigation and search", () => {
  it("开始下一次搜索后，上一项的延迟定位不会抢走焦点或关闭结果", async () => {
    render(<ThemeProvider><SettingsPanel open onClose={() => {}} /></ThemeProvider>);
    await screen.findByRole("heading", { name: "通用", level: 2 });
    const frames: FrameRequestCallback[] = [];
    const requestFrame = vi.spyOn(window, "requestAnimationFrame").mockImplementation(callback => {
      frames.push(callback);
      return frames.length;
    });
    try {
      const search = screen.getByRole("combobox", { name: "搜索设置" });
      search.focus();
      fireEvent.change(search, { target: { value: "字号" } });
      fireEvent.keyDown(search, { key: "Enter" });
      expect(search).toHaveValue("");
      expect(frames.length).toBeGreaterThan(0);

      search.focus();
      fireEvent.change(search, { target: { value: "常见问题" } });
      act(() => { for (const callback of frames.splice(0)) callback(0); });

      expect(search).toHaveFocus();
      expect(search).toHaveValue("常见问题");
      expect(screen.getByRole("listbox", { name: "设置搜索结果" })).toBeVisible();
      fireEvent.click(screen.getByRole("option", { name: /常见问题与使用帮助/ }));
      expect(await screen.findByText("确认模型配置", { exact: true })).toBeVisible();
    } finally {
      requestFrame.mockRestore();
    }
  });

  it("权限输入、规则列表和保存中的请求都受到离开保护", async () => {
    vi.mocked(permissionList).mockResolvedValue([]);
    const onClose = vi.fn();
    render(<ThemeProvider><SettingsPanel open initialSection="security" onClose={onClose} /></ThemeProvider>);
    fireEvent.click(screen.getByText("高级权限规则"));
    const pattern = screen.getByPlaceholderText("例如 git *");
    await waitFor(() => expect(pattern).toBeEnabled());
    fireEvent.change(pattern, { target: { value: "git status" } });
    expect(screen.getByRole("button", { name: "保存修改" })).toBeDisabled();
    fireEvent.click(screen.getByRole("button", { name: "通用" }));
    let dialog = await screen.findByRole("alertdialog", { name: "舍弃未保存的修改？" });
    fireEvent.click(within(dialog).getByRole("button", { name: "取消" }));
    expect(pattern).toHaveValue("git status");
    fireEvent.click(screen.getByRole("button", { name: "添加规则" }));
    fireEvent.click(screen.getByRole("button", { name: "关闭设置" }));
    dialog = await screen.findByRole("alertdialog", { name: "舍弃未保存的修改？" });
    fireEvent.click(within(dialog).getByRole("button", { name: "取消" }));

    let release!: () => void;
    vi.mocked(permissionSave).mockImplementationOnce(() => new Promise<void>(resolve => { release = resolve; }));
    fireEvent.click(screen.getByRole("button", { name: "保存修改" }));
    fireEvent.click(screen.getByRole("button", { name: "通用" }));
    expect(screen.getByText(/正在处理，请等待完成/)).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "安全与权限", level: 2 })).toBeInTheDocument();
    expect(onClose).not.toHaveBeenCalled();
    await act(async () => release());
    expect(screen.queryByText(/正在处理，请等待完成/)).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "通用" }));
    expect(screen.getByRole("heading", { name: "通用", level: 2 })).toBeInTheDocument();
    expect(screen.queryByRole("alertdialog")).toBeNull();
  });

  it("保存失败后清除等待提示并保留未保存的权限规则", async () => {
    vi.mocked(permissionList).mockResolvedValueOnce([]);
    let fail!: (error: Error) => void;
    vi.mocked(permissionSave).mockImplementationOnce(() => new Promise<void>((_resolve, reject) => { fail = reject; }));
    render(<ThemeProvider><SettingsPanel open initialSection="security" onClose={() => {}} /></ThemeProvider>);
    fireEvent.click(screen.getByText("高级权限规则"));
    const pattern = screen.getByPlaceholderText("例如 git *");
    await waitFor(() => expect(pattern).toBeEnabled());
    fireEvent.change(pattern, { target: { value: "git status" } });
    fireEvent.click(screen.getByRole("button", { name: "添加规则" }));
    fireEvent.click(screen.getByRole("button", { name: "保存修改" }));
    fireEvent.click(screen.getByRole("button", { name: "通用" }));
    expect(screen.getByText(/正在处理，请等待完成/)).toBeInTheDocument();

    await act(async () => fail(new Error("权限配置不可写")));
    expect(screen.getByRole("alert")).toHaveTextContent("权限配置不可写");
    expect(screen.queryByText(/正在处理，请等待完成/)).toBeNull();
    expect(screen.getByText("git status")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "通用" }));
    fireEvent.click(within(await screen.findByRole("alertdialog")).getByRole("button", { name: "取消" }));
    expect(screen.getByText("git status")).toBeInTheDocument();
  });

  it("并行设置操作全部完成后才清除等待提示并允许离开", async () => {
    let finishSave!: (depth: number) => void;
    let finishReload!: () => void;
    vi.mocked(subagentsConfigSave).mockImplementationOnce(() => new Promise<number>(resolve => { finishSave = resolve; }));
    vi.mocked(internalReload).mockImplementationOnce(() => new Promise<void>(resolve => { finishReload = resolve; }));
    const onClose = vi.fn();
    render(<ThemeProvider><SettingsPanel open initialSection="agent-settings" onClose={onClose} /></ThemeProvider>);
    fireEvent.click(screen.getByText("开发者选项"));
    const depth = screen.getByRole("spinbutton", { name: "子代理最大层级" });
    await waitFor(() => expect(depth).toBeEnabled());
    fireEvent.change(depth, { target: { value: "3" } });
    fireEvent.click(within(depth.closest(".agent-runtime-row") as HTMLElement).getByRole("button", { name: "保存" }));
    fireEvent.click(screen.getByText("配置未生效？"));
    fireEvent.click(screen.getByRole("button", { name: "重新加载MCP 连接器" }));
    fireEvent.click(screen.getByRole("button", { name: "通用" }));
    expect(screen.getByText(/正在处理，请等待完成/)).toBeInTheDocument();

    await act(async () => finishSave(3));
    expect(depth).toHaveValue(3);
    expect(screen.getByText(/正在处理，请等待完成/)).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "关闭设置" }));
    expect(onClose).not.toHaveBeenCalled();

    await act(async () => finishReload());
    expect(screen.queryByText(/正在处理，请等待完成/)).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "通用" }));
    expect(screen.getByRole("heading", { name: "通用", level: 2 })).toBeInTheDocument();
    expect(screen.queryByRole("alertdialog")).toBeNull();
  });

  it("输入法确认和取消候选词不会跳转设置或关闭弹窗", () => {
    const onClose = vi.fn();
    render(<ThemeProvider><SettingsPanel open onClose={onClose} /></ThemeProvider>);
    const search = screen.getByRole("combobox", { name: "搜索设置" });
    search.focus();
    fireEvent.change(search, { target: { value: "模型" } });
    for (const event of [{ isComposing: true }, { keyCode: 229 }]) {
      fireEvent.keyDown(search, { key: "Enter", ...event });
      fireEvent.keyDown(search, { key: "Escape", ...event });
      expect(search).toHaveValue("模型");
      expect(search).toHaveAttribute("aria-expanded", "true");
      expect(screen.getByRole("button", { name: "通用" })).toHaveAttribute("aria-current", "page");
    }
    expect(onClose).not.toHaveBeenCalled();
    fireEvent.keyDown(search, { key: "Escape" });
    expect(search).toHaveAttribute("aria-expanded", "false");
    expect(onClose).not.toHaveBeenCalled();
  });

  it("通知渠道草稿在子视图、搜索和微信跳转时保留，确认后才能离开", async () => {
    render(<ThemeProvider><SettingsPanel open initialSection="notify-channels" onClose={() => {}} /></ThemeProvider>);
    fireEvent.click(screen.getByRole("button", { name: /推送到其他应用/ }));
    const add = screen.getByRole("button", { name: "添加渠道" });
    await waitFor(() => expect(add).toBeEnabled());
    fireEvent.click(add);
    fireEvent.change(screen.getByRole("textbox", { name: "通知渠道显示名" }), { target: { value: "日报" } });
    fireEvent.click(screen.getByRole("button", { name: "通知记录" }));
    fireEvent.click(within(await screen.findByRole("alertdialog")).getByRole("button", { name: "取消" }));
    fireEvent.change(screen.getByRole("combobox", { name: "搜索设置" }), { target: { value: "备份" } });
    fireEvent.click(screen.getByRole("option", { name: /数据备份与恢复/ }));
    fireEvent.click(within(await screen.findByRole("alertdialog")).getByRole("button", { name: "取消" }));
    expect(screen.getByRole("textbox", { name: "通知渠道显示名" })).toHaveValue("日报");
    fireEvent.click(within(screen.getByText(/将任务事件发送到已配置的外部应用/)).getByRole("button", { name: "微信连接" }));
    fireEvent.click(within(await screen.findByRole("alertdialog")).getByRole("button", { name: "舍弃修改" }));
    await screen.findByRole("heading", { name: "微信连接", level: 2 });
  });

  it("外部直达链接也保护未保存的高级配置，并允许撤销修改", async () => {
    vi.mocked(subagentsConfigGet).mockResolvedValueOnce({ maxDepth: 2 });
    const view = render(<ThemeProvider><SettingsPanel open initialSection="agent-settings" onClose={() => {}} /></ThemeProvider>);
    fireEvent.click(screen.getByText("开发者选项"));
    const depth = screen.getByRole("spinbutton", { name: "子代理最大层级" });
    await waitFor(() => expect(depth).toHaveValue(2));
    fireEvent.change(depth, { target: { value: "4" } });
    expect(screen.queryByRole("button", { name: "刷新状态" })).toBeNull();
    view.rerender(<ThemeProvider><SettingsPanel open initialSection="help" onClose={() => {}} /></ThemeProvider>);
    fireEvent.click(within(await screen.findByRole("alertdialog")).getByRole("button", { name: "取消" }));
    expect(depth).toHaveValue(4);
    fireEvent.click(screen.getByRole("button", { name: "撤销修改" }));
    expect(depth).toHaveValue(2);
    fireEvent.click(screen.getByRole("button", { name: "帮助与更新" }));
    expect(screen.getByRole("heading", { name: "帮助与更新", level: 2 })).toBeInTheDocument();
  });

  it("搜索定位真实配置并展开折叠区域，Escape 先关闭结果列表", async () => {
    const onClose = vi.fn();
    render(<ThemeProvider><SettingsPanel open onClose={onClose} /></ThemeProvider>);
    const search = screen.getByRole("combobox", { name: "搜索设置" });
    fireEvent.focus(search);
    fireEvent.change(search, { target: { value: "遥测" } });
    fireEvent.keyDown(search, { key: "Escape" });
    expect(search).toHaveAttribute("aria-expanded", "false");
    expect(onClose).not.toHaveBeenCalled();
    fireEvent.change(search, { target: { value: "OTLP" } });
    fireEvent.keyDown(search, { key: "Enter" });
    const field = await screen.findByRole("textbox", { name: "监控服务地址（OTLP）" });
    await waitFor(() => expect(field.closest("details")).toHaveAttribute("open"));
    expect(screen.getByRole("button", { name: "高级设置" })).toHaveAttribute("aria-current", "page");
    expect(search).toHaveValue("");

    fireEvent.change(search, { target: { value: "Webhook" } });
    fireEvent.keyDown(search, { key: "Enter" });
    await waitFor(() => expect(screen.getByRole("button", { name: /推送到其他应用/ })).toHaveAttribute("aria-expanded", "true"));
    fireEvent.change(search, { target: { value: "常见问题" } });
    fireEvent.keyDown(search, { key: "Enter" });
    const faq = screen.getByText("问题排查").closest("details")!;
    await waitFor(() => expect(faq).toHaveAttribute("open"));
    expect(screen.getByText("确认模型配置")).toBeVisible();
    await waitFor(() => expect(faq.querySelector("summary")).toHaveFocus());
    fireEvent.change(search, { target: { value: "不存在的设置" } });
    expect(screen.getByText(/没有找到相关设置/)).toBeInTheDocument();
  });
});

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

    expect(await screen.findByRole("heading", { name: "记忆" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "记忆" })).toHaveAttribute("aria-current", "page");
    expect(screen.queryByRole("button", { name: "记忆行为" })).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "管理" }));
    fireEvent.click(screen.getByRole("button", { name: "更多" }));
    fireEvent.click(screen.getByRole("menuitem", { name: "原有记忆文档" }));
    await waitFor(() => expect(memoryList).toHaveBeenCalledWith("/workspace"));
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
    const focusable = Array.from(dialog.querySelectorAll<HTMLElement>("button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled])"));
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

  it("按使用场景分组设置，默认进入通用，常用偏好聚合为子页面", async () => {
    const { container } = renderSettings();

    for (const group of ["偏好", "智能体", "连接与数据", "关于与支持"]) {
      expect(screen.getByRole("heading", { name: group, level: 2 })).toBeInTheDocument();
    }

    expect(container.querySelectorAll(".settings-navigation__item")).toHaveLength(11);
    expect(screen.getByRole("button", { name: "模型用量" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "通知" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "微信连接" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "云存储" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "通用" })).toHaveAttribute("aria-current", "page");
    expect(await screen.findByRole("heading", { name: "通用", level: 2 })).toBeInTheDocument();
    expect(screen.queryByText("MCP 连接器")).not.toBeInTheDocument();
    expect(screen.getByRole("group", { name: "界面主题" })).toBeInTheDocument();
    expect(screen.getByRole("slider", { name: "字体大小" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "通用" })).toHaveAttribute("aria-current", "page");
    fireEvent.click(screen.getByRole("button", { name: "快捷键" }));
    expect(screen.getByRole("heading", { name: "快捷键", level: 2 })).toBeInTheDocument();

  });

  it("旧的外观和快捷键直达入口仍归属通用", async () => {
    const { rerender } = render(<ThemeProvider><SettingsPanel open initialSection="personalize" onClose={() => {}} /></ThemeProvider>);
    expect(screen.getByRole("button", { name: "通用" })).toHaveAttribute("aria-current", "page");
    expect(screen.getByRole("button", { name: "常用偏好" })).toHaveAttribute("aria-pressed", "true");
    rerender(<ThemeProvider><SettingsPanel open initialSection="shortcuts" onClose={() => {}} /></ThemeProvider>);
    expect(await screen.findByRole("heading", { name: "快捷键", level: 2 })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "通用" })).toHaveAttribute("aria-current", "page");
  });

  it("配置重载和诊断集中在高级设置，保存诊断仍校验端点", async () => {
    localStorage.removeItem("echoagent.otlp.endpoint");
    render(<ThemeProvider><SettingsPanel open initialSection="security" onClose={() => {}} /></ThemeProvider>);
    expect(screen.queryByLabelText("监控服务地址（OTLP）")).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "高级设置" }));
    expect(await screen.findByText("配置未生效？")).toBeInTheDocument();
    const disclosure = screen.getByText("开发者选项").closest("details")!;
    expect(disclosure).not.toHaveAttribute("open");
    fireEvent.click(screen.getByText("开发者选项"));
    const field = screen.getByRole("textbox", { name: "监控服务地址（OTLP）" });
    fireEvent.change(field, { target: { value: "javascript:invalid" } });
    fireEvent.click(within(screen.getByRole("group", { name: "监控服务配置" })).getByRole("button", { name: "保存" }));
    expect(screen.getByRole("alert")).toHaveTextContent("无法保存监控配置");
    expect(localStorage.getItem("echoagent.otlp.endpoint")).toBeNull();
    fireEvent.change(field, { target: { value: "http://127.0.0.1:4318/v1/logs" } });
    fireEvent.click(within(screen.getByRole("group", { name: "监控服务配置" })).getByRole("button", { name: "保存" }));
    expect(localStorage.getItem("echoagent.otlp.endpoint")).toBe("http://127.0.0.1:4318/v1/logs");
    localStorage.removeItem("echoagent.otlp.endpoint");
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

    fireEvent.click(await screen.findByRole("link", { name: /使用文档/ }));

    await waitFor(() => {
      expect(openExternalUrl).toHaveBeenCalledWith("https://fuyuxiang.github.io/echo-agent/");
    });
  });

  it("所有设置入口都能进入对应页面并更新当前页状态", async () => {
    renderSettings();

    const pages = [
      ["通用", "通用"], ["通知", "通知设置"], ["微信连接", "微信连接"],
      ["模型", "模型"], ["高级设置", "高级设置"], ["记忆", "记忆"],
      ["模型用量", "模型用量"], ["云存储", "云存储"], ["数据管理", "备份与恢复"],
      ["安全与权限", "安全与权限"], ["帮助与更新", "帮助与更新"],
    ];
    for (const [label, heading] of pages) {
      const navigationItem = within(screen.getByRole("navigation")).getByRole("button", { name: label });
      fireEvent.click(navigationItem);
      expect(await screen.findByRole("heading", { name: heading, level: 2 })).toBeInTheDocument();
      expect(navigationItem).toHaveAttribute("aria-current", "page");
    }
  }, 15_000);

  it("通知、记忆和数据在一个一级入口下保留完整子页面", async () => {
    renderSettings();
    fireEvent.click(screen.getByRole("button", { name: "通知" }));
    fireEvent.click(screen.getByRole("button", { name: "通知设置" }));
    expect(await screen.findByRole("heading", { name: "通知设置" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "通知" })).toHaveAttribute("aria-current", "page");
    expect(screen.queryByRole("button", { name: "微信远程对话" })).not.toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "记忆" }));
    expect(screen.queryByRole("button", { name: "个人记忆" })).not.toBeInTheDocument();
    expect(await screen.findByRole("heading", { name: "记忆" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "记忆" })).toHaveAttribute("aria-current", "page");

    fireEvent.click(screen.getByRole("button", { name: "数据管理" }));
    fireEvent.click(screen.getByRole("button", { name: "已归档会话" }));
    expect(await screen.findByRole("heading", { name: "已归档会话" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "数据管理" })).toHaveAttribute("aria-current", "page");
  });

  it("消息通道独立于通知视图，直接打开时也高亮正确入口", async () => {
    const { rerender } = render(
      <ThemeProvider><SettingsPanel open initialSection="weixin-channel" onClose={() => {}} /></ThemeProvider>,
    );
    expect(screen.getByRole("button", { name: "微信连接" })).toHaveAttribute("aria-current", "page");
    expect(screen.getByRole("button", { name: "通知" })).not.toHaveAttribute("aria-current");
    expect(screen.getByRole("heading", { name: "微信连接", level: 2 })).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "微信远程对话", level: 3 })).toBeInTheDocument();
    expect(screen.getByTestId("weixin-channel-panel")).toBeInTheDocument();
    expect(screen.queryByRole("group", { name: "通知视图" })).not.toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "通知" }));
    fireEvent.click(screen.getByRole("button", { name: "通知记录" }));
    expect(await screen.findByRole("heading", { name: "通知记录" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "通知" })).toHaveAttribute("aria-current", "page");
    expect(screen.queryByTestId("weixin-channel-panel")).not.toBeInTheDocument();

    rerender(<ThemeProvider><SettingsPanel open={false} initialSection="weixin-channel" onClose={() => {}} /></ThemeProvider>);
    rerender(<ThemeProvider><SettingsPanel open initialSection="weixin-channel" onClose={() => {}} /></ThemeProvider>);
    expect(screen.getByRole("button", { name: "微信连接" })).toHaveAttribute("aria-current", "page");
  });

  it("安全中心说明浏览器与电脑操作按任务开启，不设置高风险全局默认值", async () => {
    render(
      <ThemeProvider>
        <SettingsPanel open initialSection="security" onClose={() => {}} />
      </ThemeProvider>,
    );

    expect(await screen.findByRole("heading", { name: "操作权限", level: 3 }))
      .toBeInTheDocument();
    expect(screen.getByText(/在聊天中说明电脑任务即可开始/)).toBeInTheDocument();
    expect(screen.queryByRole("checkbox", { name: /默认.*操作电脑/ })).toBeNull();
  });

  it("通知设置和通知记录分开，记录页不重复显示偏好与概览", async () => {
    renderSettings();

    expect(screen.queryByRole("button", { name: "助理设置" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "账户管理" })).not.toBeInTheDocument();
    expect(screen.queryByText("智能体邮箱")).not.toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "通知" }));
    fireEvent.click(screen.getByRole("button", { name: "通知记录" }));
    expect(await screen.findByRole("heading", { name: "通知记录" })).toBeInTheDocument();
    expect(screen.queryByRole("heading", { name: "通知概览" })).toBeNull();
    expect(screen.queryByRole("switch", { name: "在通知中显示任务标题" })).toBeNull();
    expect(screen.getByRole("heading", { name: "最近通知", level: 3 })).toBeInTheDocument();
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
    fireEvent.click(screen.getByRole("button", { name: "需要处理" }));
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
    fireEvent.click(screen.getByRole("button", { name: "通知设置" }));
    const preview = await screen.findByRole("switch", { name: "在通知中显示任务标题" });
    await waitFor(() => expect(preview).toBeEnabled());
    fireEvent.click(preview);
    await waitFor(() => expect(desktopNotificationPreviewSave).toHaveBeenCalledWith(true));
    vi.mocked(notificationList).mockResolvedValueOnce([{ id: 10, kind: "session_complete", at: "2026-09-04T20:00:00+08:00", title: "任务《整理报告》的回复已完成", severity: "info", read: false, sessionId: "session-456" }]);
    fireEvent.click(screen.getByRole("button", { name: "通知记录" }));
    fireEvent.click(await screen.findByRole("button", { name: "打开相关任务" }));
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
    for (const input of screen.getAllByRole("switch")) expect(input).toBeDisabled();
    fireEvent.click(screen.getByRole("switch", { name: "使用记忆" }));
    expect(memoryConfigSave).not.toHaveBeenCalled();
    vi.mocked(memoryConfigGet).mockResolvedValueOnce({ enabled: true, initialInjectionEnabled: false, saveOnEnd: false, watcherEnabled: false, autoFlushEnabled: false, dreamEnabled: false, revision: "r1" });
    fireEvent.click(screen.getByRole("button", { name: "重新加载" }));
    await waitFor(() => expect(screen.getByRole("switch", { name: "使用记忆" })).toBeEnabled());
    fireEvent.click(screen.getByRole("switch", { name: "使用记忆" }));
    await waitFor(() => expect(memoryConfigSave).toHaveBeenCalledWith({ enabled: false }, "r1"));
    expect(screen.queryByRole("checkbox", { name: "监听外部修改" })).not.toBeInTheDocument();
  });

  it("旧版关闭策略按原选择保留，可明确恢复推荐策略", async () => {
    vi.mocked(memoryConfigGet).mockResolvedValueOnce({ enabled: true, initialInjectionEnabled: true, saveOnEnd: true, watcherEnabled: true, autoFlushEnabled: false, dreamEnabled: true, revision: "legacy" });
    vi.mocked(memoryConfigSave).mockClear();
    render(<ThemeProvider><SettingsPanel open initialSection="memory" onClose={() => {}} /></ThemeProvider>);
    const restore = await screen.findByRole("button", { name: "恢复推荐策略" });
    expect(memoryConfigSave).not.toHaveBeenCalled();
    expect(screen.getAllByRole("switch")).toHaveLength(1);
    fireEvent.click(restore);
    await waitFor(() => expect(memoryConfigSave).toHaveBeenCalledWith({ enabled: true, initialInjectionEnabled: true, saveOnEnd: true, watcherEnabled: true, autoFlushEnabled: true, dreamEnabled: true }, "legacy"));
  });

  it("离开记忆页先保护草稿，取消离开保留编辑内容", async () => {
    render(<ThemeProvider><SettingsPanel open initialSection="memory" onClose={() => {}} /></ThemeProvider>);
    fireEvent.click(await screen.findByRole("button", { name: "管理" }));
    fireEvent.click(screen.getByRole("button", { name: "更多" }));
    fireEvent.click(screen.getByRole("menuitem", { name: "添加记忆" }));
    fireEvent.change(screen.getByLabelText("记忆内容"), { target: { value: "汇报先给结论" } });
    fireEvent.click(screen.getByRole("button", { name: "通用" }));
    const confirmation = await screen.findByRole("alertdialog", { name: "舍弃未保存的记忆？" });
    fireEvent.click(within(confirmation).getByRole("button", { name: "取消" }));
    expect(screen.getByLabelText("记忆内容")).toHaveValue("汇报先给结论");
    fireEvent.click(screen.getByRole("button", { name: "通用" }));
    fireEvent.click(within(await screen.findByRole("alertdialog", { name: "舍弃未保存的记忆？" })).getByRole("button", { name: "舍弃修改" }));
    expect(screen.queryByLabelText("记忆内容")).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "通用" })).toHaveAttribute("aria-current", "page");
  });

  it("记忆二级页支持搜索直达，搜索离开编辑时仍保护草稿", async () => {
    render(<ThemeProvider><SettingsPanel open initialSection="memory" onClose={() => {}} /></ThemeProvider>);
    fireEvent.click(await screen.findByRole("button", { name: "管理" }));
    const search = screen.getByRole("combobox", { name: "搜索设置" });
    fireEvent.change(search, { target: { value: "使用记忆" } });
    fireEvent.click(await screen.findByRole("option", { name: /使用记忆/ }));
    expect(await screen.findByRole("switch", { name: "使用记忆" })).toBeInTheDocument();
    fireEvent.change(search, { target: { value: "管理记忆" } });
    fireEvent.click(await screen.findByRole("option", { name: /管理记忆/ }));
    expect(await screen.findByRole("heading", { name: "管理记忆" })).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "更多" }));
    fireEvent.click(screen.getByRole("menuitem", { name: "添加记忆" }));
    fireEvent.change(screen.getByLabelText("记忆内容"), { target: { value: "不能丢失的记忆草稿" } });
    fireEvent.change(search, { target: { value: "使用记忆" } });
    fireEvent.click(await screen.findByRole("option", { name: /使用记忆/ }));
    const confirmation = await screen.findByRole("alertdialog", { name: "舍弃未保存的记忆？" });
    fireEvent.click(within(confirmation).getByRole("button", { name: "取消" }));
    expect(screen.getByLabelText("记忆内容")).toHaveValue("不能丢失的记忆草稿");
  });

  it("点击模型导航返回首页前保护连接草稿", async () => {
    render(<ThemeProvider><SettingsPanel open initialSection="model" onClose={() => {}} /></ThemeProvider>);
    fireEvent.click(await screen.findByRole("button", { name: "添加模型连接" }));
    fireEvent.change(screen.getByLabelText("模型 ID"), { target: { value: "keep-draft" } });
    fireEvent.click(within(screen.getByRole("navigation")).getByRole("button", { name: "模型" }));
    const warning = await screen.findByRole("alertdialog", { name: "舍弃未保存的修改？" });
    fireEvent.click(within(warning).getByRole("button", { name: "取消" }));
    expect(screen.getByLabelText("模型 ID")).toHaveValue("keep-draft");
    fireEvent.click(within(screen.getByRole("navigation")).getByRole("button", { name: "模型" }));
    fireEvent.click(within(await screen.findByRole("alertdialog")).getByRole("button", { name: "舍弃修改" }));
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "添加模型连接" })).toBeNull());
    expect(screen.getByRole("heading", { name: "模型", level: 2 })).toBeInTheDocument();
  });

  it.each(["navigation", "search"] as const)("模型直达后通过 %s 返回模型首页不再重复定位旧模型", async route => {
    const catalog = {
      providers: [{ id: "review", providerKind: "custom" as const, source: "personal" as const, label: "工作连接", baseUrl: "https://review.invalid/v1", credentialConfigured: true }],
      models: [{ modelId: "review/a", providerId: "review" }, { modelId: "review/b", providerId: "review" }],
    };
    vi.mocked(providersList).mockResolvedValueOnce(catalog).mockResolvedValueOnce(catalog);
    const props = { open: true, initialSection: "model" as const, initialModelId: "review/a", onClose: () => {} };
    const view = render(<ThemeProvider><SettingsPanel {...props} /></ThemeProvider>);
    await screen.findByRole("heading", { name: "连接详情" });
    await waitFor(() => expect(document.querySelector('[data-model-id="review/a"]')).toHaveFocus());
    fireEvent.click(screen.getByRole("button", { name: "通用" }));
    if (route === "navigation") fireEvent.click(within(screen.getByRole("navigation")).getByRole("button", { name: "模型" }));
    else {
      fireEvent.change(screen.getByRole("combobox", { name: "搜索设置" }), { target: { value: "模型与连接" } });
      fireEvent.click(await screen.findByRole("option", { name: /模型与连接/ }));
    }
    expect(await screen.findByRole("combobox", { name: "默认模型" })).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "模型" })).toBeInTheDocument();
    expect(screen.queryByRole("heading", { name: "连接详情" })).toBeNull();
    // A later explicit direct link still locates its requested model.
    view.rerender(<ThemeProvider><SettingsPanel {...props} initialModelId="review/b" /></ThemeProvider>);
    expect(await screen.findByRole("heading", { name: "连接详情" })).toBeInTheDocument();
    await waitFor(() => expect(document.querySelector('[data-model-id="review/b"]')).toHaveFocus());
  });

  it("记忆开关仅保存更改字段", async () => {
    render(
      <ThemeProvider>
        <SettingsPanel open initialSection="memory" onClose={() => {}} />
      </ThemeProvider>,
    );

    const autoFlush = await screen.findByRole("switch", { name: "使用记忆" });
    expect(screen.queryByLabelText("记忆检索方式")).not.toBeInTheDocument();
    expect(autoFlush).toBeChecked();
    fireEvent.click(autoFlush);

    await waitFor(() => {
      expect(memoryConfigSave).toHaveBeenCalledWith({ enabled: false }, undefined);
    });
    expect(
      screen.getByText("记忆配置已保存，立即用于后续提取和模型请求。"),
    ).toBeInTheDocument();
  });

  it("默认关闭到托盘，并可在系统设置中关闭", async () => {
    render(
      <ThemeProvider>
        <SettingsPanel open initialSection="general" onClose={() => {}} />
      </ThemeProvider>,
    );

    const toggle = await screen.findByRole("switch", { name: "关闭窗口后继续运行" });
    await waitFor(() => expect(toggle).toBeEnabled());
    expect(toggle).toBeChecked();

    fireEvent.click(toggle);

    await waitFor(() => {
      expect(desktopPreferencesSave).toHaveBeenCalledWith(false);
      expect(toggle).not.toBeChecked();
    });
    expect(screen.getByRole("status")).toHaveTextContent("关闭主窗口将停止任务执行和自动化调度");
    expect(screen.getByText("已关闭后台运行，下次关闭窗口将退出 EchoAgent。")).toBeInTheDocument();
  });

  it("桌面偏好保存失败时回滚后台运行开关", async () => {
    vi.mocked(desktopPreferencesSave).mockRejectedValueOnce(new Error("偏好文件只读"));
    render(
      <ThemeProvider>
        <SettingsPanel open initialSection="general" onClose={() => {}} />
      </ThemeProvider>,
    );

    const toggle = await screen.findByRole("switch", { name: "关闭窗口后继续运行" });
    await waitFor(() => expect(toggle).toBeEnabled());
    fireEvent.click(toggle);

    await waitFor(() => expect(toggle).toBeChecked());
    expect(screen.getByText("保存桌面偏好失败：偏好文件只读")).toBeInTheDocument();
  });

  it("桌面偏好读取失败时不允许覆盖未知配置，重试后恢复操作", async () => {
    vi.mocked(desktopPreferencesGet).mockRejectedValueOnce(new Error("偏好文件不可读"));
    render(<ThemeProvider><SettingsPanel open initialSection="general" onClose={() => {}} /></ThemeProvider>);
    const toggle = await screen.findByRole("switch", { name: "关闭窗口后继续运行" });
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
    expect(await screen.findByText("子代理最大层级")).toBeInTheDocument();
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
    expect(screen.getByRole("button", { name: "保存修改" })).toBeDisabled();
    expect(permissionSave).not.toHaveBeenCalled();

    fireEvent.click(screen.getByRole("button", { name: "重试" }));
    await waitFor(() => expect(screen.queryByRole("alert")).not.toBeInTheDocument());
    expect(screen.getByText("rm *")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "保存修改" })).toBeDisabled();
  });

  it("保存规则后收起面板仍会展开失败提示并保留规则", async () => {
    vi.mocked(permissionList).mockResolvedValue([{action:"deny",tool:"bash",pattern:"rm *"}]);
    let fail!: (error: Error) => void;
    vi.mocked(permissionSave).mockImplementationOnce(() => new Promise((_resolve, reject) => { fail = reject; }));
    render(<ThemeProvider><SettingsPanel open initialSection="security" onClose={() => {}} /></ThemeProvider>);
    const summary = await screen.findByText("高级权限规则", {exact:true});
    fireEvent.click(summary);
    fireEvent.change(screen.getByPlaceholderText("例如 git *"), { target: { value: "git status" } });
    fireEvent.click(screen.getByRole("button", {name:"添加规则"}));
    fireEvent.click(await screen.findByRole("button", {name:"保存修改"}));
    fireEvent.click(summary);
    await act(async () => { fail(new Error("disk unavailable")); });
    expect(await screen.findByRole("alert")).toHaveTextContent("保存失败");
    expect(summary.closest("details")).toHaveAttribute("open");
    expect(screen.getByText("rm *")).toBeVisible();
  });

  it("在设置中集中查看、恢复并打开归档会话", async () => {
    useSessionsStore.setState({ deletedSessionIds: [],
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

    expect(screen.getByRole("heading", { name: "已归档会话", level: 2 })).toBeInTheDocument();
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
    useSessionsStore.setState({ deletedSessionIds: [],
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
    useSessionsStore.setState({ deletedSessionIds: [],
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
    useSessionsStore.setState({ deletedSessionIds: [],
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


it("通知一级入口直接提供桌面提醒和带当前任务目标的测试", async () => {
  render(<ThemeProvider><SettingsPanel open onClose={() => {}} sessionId="active-task" /></ThemeProvider>);
  fireEvent.click(screen.getByRole("button", {name:"通知"}));
  expect(await screen.findByText(/已开启 · 默认设置/)).toBeInTheDocument();
  fireEvent.click(screen.getByRole("button", {name:"发送测试通知"}));
  await waitFor(() => expect(testDesktopNotify).toHaveBeenCalledWith("active-task"));
  fireEvent.click(screen.getByRole("button", {name:"通知记录"}));
  expect(await screen.findByRole("heading", {name:"通知记录"})).toBeInTheDocument();
  expect(screen.getByRole("button", {name:"通知"})).toHaveAttribute("aria-current", "page");
});

it("通知记录按用户目的分组，连接异常归入异常且不误归为需要处理", async () => {
  vi.mocked(notificationList).mockResolvedValueOnce([
    { id: 1, kind: "permission", title: "等待任务授权", severity: "warn", read: false, at: "2026-10-18T10:00:00Z" },
    { id: 2, kind: "mcp_status", title: "文档工具无法连接", severity: "warn", read: true, at: "2026-10-18T09:00:00Z" },
    { id: 3, kind: "session_complete", title: "周报完成", severity: "info", read: false, at: "2026-10-18T08:00:00Z" },
    { id: 4, kind: "models_update", title: "模型目录更新", severity: "info", read: true, at: "2026-10-18T07:00:00Z" },
  ]);
  render(<ThemeProvider><SettingsPanel open initialSection="agent-mail" onClose={() => {}} /></ThemeProvider>);
  await screen.findByText("等待任务授权");
  fireEvent.click(screen.getByRole("button", { name: "需要处理" }));
  expect(screen.getByText("等待任务授权")).toBeInTheDocument();
  expect(screen.queryByText("文档工具无法连接")).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: "异常" }));
  expect(screen.getByText("文档工具无法连接")).toBeInTheDocument();
  expect(screen.queryByText("等待任务授权")).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: "系统事件" }));
  expect(screen.getByText("模型目录更新")).toBeInTheDocument();
  expect(screen.queryByText("文档工具无法连接")).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: "未读" }));
  expect(screen.getByText("等待任务授权")).toBeInTheDocument();
  expect(screen.getByText("周报完成")).toBeInTheDocument();
  expect(screen.queryByText("模型目录更新")).toBeNull();
});

it("通知记录只将成功完成的任务结果标为成功色", async () => {
  const entries = [
    { kind: "session_complete", title: "任务《整理周报》的回复已完成", severity: "info", tone: "success", read: true },
    { kind: "session_complete", title: "自动化《发送周报》已完成", severity: "info", tone: "success", read: false },
    { kind: "session_complete", title: "自动化《归档周报》已停止", severity: "info", tone: "info", read: false },
    { kind: "session_complete", title: "自动化《被截断的长标题", body: "本次运行已停止，后续计划不受影响", severity: "info", tone: "info", read: false },
    { kind: "session_complete", title: "自动化《汇总周报》已人工接管", severity: "warn", tone: "warn", read: false },
    { kind: "error", title: "自动化《统计周报》运行失败", severity: "error", tone: "error", read: false },
    { kind: "permission", title: "任务需要授权", severity: "warn", tone: "warn", read: false },
    { kind: "models_update", title: "模型目录已更新", severity: "info", tone: "info", read: false },
  ];
  vi.mocked(notificationList).mockResolvedValueOnce(entries.map(({ tone, ...entry }, index) => ({
    ...entry, id: index + 1, at: "2026-10-18T10:00:00Z",
  })));
  render(<ThemeProvider><SettingsPanel open initialSection="agent-mail" onClose={() => {}} /></ThemeProvider>);
  await screen.findByText(entries[0].title);
  for (const { title, tone, read } of entries) {
    const row = screen.getByText(title).closest<HTMLElement>(".notification-row")!;
    expect(row.querySelector(".notification-row__dot")).toHaveClass(`notification-row__dot--${tone}`);
    expect(within(row).queryAllByText("未读")).toHaveLength(read ? 0 : 1);
  }
});

it("普通通知说明直接可读，结构化诊断信息需要展开查看", async () => {
  vi.mocked(notificationList).mockResolvedValueOnce([
    { id: 1, kind: "mcp_status", title: "工具连接异常", body: '{"status":"disconnected","detail":"internal-diagnostic"}', severity: "warn", read: false, at: "2026-10-18T10:00:00Z" },
    { id: 2, kind: "permission", title: "任务需要授权", body: "打开任务决定是否允许", severity: "warn", read: false, at: "2026-10-18T09:00:00Z" },
  ]);
  render(<ThemeProvider><SettingsPanel open initialSection="agent-mail" onClose={() => {}} /></ThemeProvider>);
  expect(await screen.findByText("打开任务决定是否允许")).toBeVisible();
  const details = screen.getByText("查看技术详情").closest("details")!;
  expect(details).not.toHaveAttribute("open");
  expect(screen.getByText(/internal-diagnostic/)).not.toBeVisible();
  fireEvent.click(screen.getByText("查看技术详情"));
  expect(screen.getByText(/internal-diagnostic/)).toBeVisible();
});
