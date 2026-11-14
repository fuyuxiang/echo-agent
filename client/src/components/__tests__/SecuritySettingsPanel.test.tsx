// @vitest-environment jsdom
import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import "@testing-library/jest-dom/vitest";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { SecuritySettingsPanel } from "../SettingsSections";
import { useSessionStore } from "@/stores/session-store";
import { useSessionsStore } from "@/stores/sessions-store";
import type { AutomationCapabilities } from "@/lib/automation-client";

const actions = vi.hoisted(() => ({ list: vi.fn(), save: vi.fn(), capabilities: vi.fn(), request: vi.fn() }));
vi.mock("@/lib/agent-client", async original => ({
  ...await original<typeof import("@/lib/agent-client")>(), permissionList: actions.list, permissionSave: actions.save,
}));
vi.mock("@/lib/automation-client", async original => ({
  ...await original<typeof import("@/lib/automation-client")>(), automationCapabilities: actions.capabilities,
  automationRequestComputerPermissions: actions.request,
}));
const ready: AutomationCapabilities = { browser: { available: true, browserName: "Chrome" }, computer: { available: true, platform: "macOS", screenCapture: true, inputControl: true, desktopAvailable: true } };
const openRules = () => fireEvent.click(screen.getByText("高级权限规则"));

beforeEach(() => {
  vi.resetAllMocks();
  Object.defineProperty(window, "__TAURI_INTERNALS__", { configurable: true, value: {} });
  useSessionStore.setState({ sessionId: null, transcripts: {}, streaming: false });
  useSessionsStore.setState({ independent: [] });
  actions.list.mockResolvedValue([]); actions.save.mockResolvedValue(undefined);
  actions.capabilities.mockResolvedValue(ready); actions.request.mockResolvedValue(ready.computer);
});

describe("安全与权限设置", () => {
  it("打开页面只读取状态和规则，不开启操作或改写规则", async () => {
    const stored = [{ action: "deny", tool: "bash", pattern: "rm *" }, { action: "ask", tool: "custom-plugin", pattern: "existing:*" }];
    actions.list.mockResolvedValue(stored);
    const { container } = render(<SecuritySettingsPanel />);
    expect(await screen.findByText("已授权")).toBeInTheDocument();
    expect(screen.getByRole("group", { name: "浏览器操作状态" })).toHaveTextContent("已就绪");
    expect(screen.getByText(/在聊天中说明电脑任务即可开始/)).toBeVisible();
    expect(container.querySelectorAll("details[open]")).toHaveLength(0);
    openRules();
    const list = screen.getByRole("list", { name: "权限规则列表" });
    expect(list).toHaveTextContent("拒绝执行命令rm *"); expect(list).toHaveTextContent("询问custom-pluginexisting:*");
    expect(screen.getByRole("button", { name: "保存修改" })).toBeDisabled();
    expect(actions.request).not.toHaveBeenCalled(); expect(actions.save).not.toHaveBeenCalled();
  });

  it("系统支持但缺少权限时明确缺失项目，仅主动点击才发起授权", async () => {
    actions.capabilities.mockResolvedValue({ ...ready, computer: { ...ready.computer, screenCapture: false, inputControl: false } });
    let finish!: () => void;
    actions.request.mockImplementationOnce(() => new Promise<void>(resolve => { finish = resolve; }));
    render(<SecuritySettingsPanel />);
    expect(await screen.findByText("需要授权")).toBeInTheDocument();
    expect(screen.getByRole("group", { name: "电脑操作状态" })).toHaveTextContent("缺少系统权限：屏幕录制、辅助功能");
    expect(actions.request).not.toHaveBeenCalled();
    const button = screen.getByRole("button", { name: "前往授权" });
    fireEvent.click(button); fireEvent.click(button);
    expect(actions.request).toHaveBeenCalledOnce(); expect(button).toBeDisabled();
    actions.capabilities.mockResolvedValue(ready);
    await act(async () => finish());
    expect(await screen.findByText("已授权")).toBeInTheDocument();
    expect(actions.save).not.toHaveBeenCalled();
  });

  it("桌面不可用时不误报已授权，也不提供无效的授权按钮", async () => {
    actions.capabilities.mockResolvedValue({ ...ready, computer: { ...ready.computer, desktopAvailable: false, reason: "请解锁当前桌面" } });
    render(<SecuritySettingsPanel />);
    expect(await screen.findByText("暂不可用")).toBeInTheDocument();
    expect(screen.getByRole("group", { name: "电脑操作状态" })).toHaveTextContent("请解锁当前桌面");
    expect(screen.queryByText("已授权")).toBeNull();
    expect(screen.queryByRole("button", { name: "前往授权" })).toBeNull();
  });

  it("Windows 就绪不冒充 macOS 授权，未就绪时显示实际原因", async () => {
    actions.capabilities.mockResolvedValue({ ...ready, computer: { ...ready.computer, platform: "Windows" } });
    render(<SecuritySettingsPanel />);
    const computer = screen.getByRole("group", { name: "电脑操作状态" });
    await waitFor(() => expect(computer).toHaveTextContent("已就绪"));
    expect(computer).not.toHaveTextContent("已授权");
    actions.capabilities.mockResolvedValue({ ...ready, computer: { ...ready.computer, platform: "Windows", inputControl: false, reason: "当前会话不可操作" } });
    fireEvent.click(screen.getByRole("button", { name: "重新检查权限" }));
    await waitFor(() => expect(computer).toHaveTextContent("当前会话不可操作"));
    expect(screen.queryByRole("button", { name: "前往授权" })).toBeNull();
  });

  it("检查失败不显示缓存成功状态，重试和返回窗口仍沿用读取接口", async () => {
    actions.capabilities.mockRejectedValueOnce(new Error("状态暂不可读"));
    render(<SecuritySettingsPanel />);
    expect(await screen.findByRole("alert")).toHaveTextContent("状态暂不可读");
    expect(screen.getAllByText("检查失败")).toHaveLength(2);
    expect(screen.queryByText("已授权")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "重试检查权限" }));
    expect(await screen.findByText("已授权")).toBeInTheDocument();
    fireEvent(window, new Event("focus"));
    await waitFor(() => expect(actions.capabilities).toHaveBeenCalledTimes(3));
    expect(actions.request).not.toHaveBeenCalled(); expect(actions.save).not.toHaveBeenCalled();
  });

  it("中文展示不改变已保存的规则、顺序和自定义值，只有保存才写入", async () => {
    const original = [{ action: "deny", tool: "bash", pattern: "rm *" }, { action: "custom-action", tool: "legacy-tool", pattern: "existing:*" }];
    actions.list.mockResolvedValue(original);
    render(<SecuritySettingsPanel />); await screen.findByText("已授权"); openRules();
    fireEvent.change(screen.getByRole("combobox", { name: "工具类型" }), { target: { value: "read" } });
    fireEvent.change(screen.getByRole("textbox", { name: "匹配范围（可选）" }), { target: { value: "/work/*" } });
    fireEvent.click(screen.getByRole("button", { name: "添加规则" }));
    expect(screen.getByRole("list", { name: "权限规则列表" })).toHaveTextContent("询问读取文件/work/*");
    expect(screen.getByText("规则列表有未保存的修改。")).toBeVisible();
    expect(actions.save).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "保存修改" }));
    await waitFor(() => expect(actions.save).toHaveBeenCalledExactlyOnceWith([...original, { action: "ask", tool: "read", pattern: "/work/*" }]));
    expect(await screen.findByText(/规则已保存/)).toHaveTextContent("重启 Agent");
    expect(screen.getByRole("button", { name: "保存修改" })).toBeDisabled();
  });

  it("规则读取失败不视为空规则，恢复后不自动覆盖保存", async () => {
    actions.list.mockRejectedValueOnce(new Error("规则不可读")).mockResolvedValue([{ action: "allow", tool: "mcp", pattern: "existing:*" }]);
    render(<SecuritySettingsPanel />);
    expect(await screen.findByRole("alert")).toHaveTextContent("规则不可读");
    expect(screen.getByRole("button", { name: "保存修改" })).toBeDisabled();
    fireEvent.click(screen.getByRole("button", { name: "重试" }));
    expect(await screen.findByText("existing:*")).toBeInTheDocument();
    expect(actions.save).not.toHaveBeenCalled();
  });
});
