import { describe, it, expect, beforeEach, vi } from "vitest";
import { render, screen, fireEvent, waitFor, within } from "@testing-library/react";
import type { NotifyChannel } from "@/lib/notify-channels";

const backend = vi.hoisted(() => {
  let channels: NotifyChannel[] = [];
  return {
    reset: () => { channels = []; },
    desktopStatus: vi.fn(async () => ({enabled: !channels.some(c => c.kind === "desktop") || channels.some(c => c.kind === "desktop" && c.enabled), source: channels.some(c => c.kind === "desktop") ? "configured" : "default", permission: "unknown"})),
    desktopToggle: vi.fn(async (enabled: boolean) => {channels = [...channels.filter(c => c.kind !== "desktop"), {id:"desktop",label:"桌面提醒",kind:"desktop",enabled}];}),
    desktopTest: vi.fn(async (_sessionId?: string) => ({id:"desktop",ok:true})),
    seed: (items: NotifyChannel[]) => { channels = items; },
    load: vi.fn(async () => channels.map((item) => ({
      ...item,
      endpoint: item.endpoint ? `${new URL(item.endpoint).origin}/…` : undefined,
    }))),
    save: vi.fn(async (channel: NotifyChannel) => {
      const previous = channels.find((item) => item.id === channel.id);
      channels = [...channels.filter((item) => item.id !== channel.id), {
        ...channel,
        endpoint: channel.endpoint ?? previous?.endpoint,
      }];
    }),
    remove: vi.fn(async (id: string) => {
      channels = channels.filter((item) => item.id !== id);
    }),
    setEnabled: vi.fn(async (id: string, enabled: boolean) => {
      channels = channels.map((item) => item.id === id ? { ...item, enabled } : item);
    }),
    test: vi.fn(async (id: string) => ({ id, ok: true })),
  };
});

vi.mock("@/lib/notify-channels", () => ({
  loadNotifyChannels: backend.load,
  loadDesktopNotifyStatus: backend.desktopStatus,
  setDesktopNotifyEnabled: backend.desktopToggle,
  testDesktopNotify: backend.desktopTest,
  saveNotifyChannel: backend.save,
  removeNotifyChannel: backend.remove,
  setNotifyChannelEnabled: backend.setEnabled,
  testNotifyChannel: backend.test,
}));

vi.mock("@/lib/agent-client", () => ({
  desktopPreferencesGet: vi.fn(async () => ({ closeToTray: true, showNotificationTaskTitle: false })),
  desktopNotificationPreviewSave: vi.fn(async (showTaskTitle: boolean) => ({ closeToTray: true, showNotificationTaskTitle: showTaskTitle })),
}));
import { desktopPreferencesGet, desktopNotificationPreviewSave } from "@/lib/agent-client";

async function openAdvanced() {
  fireEvent.click(screen.getByRole("button", { name: /推送到其他应用/ }));
}
async function openAdd() {
  await openAdvanced();
  await waitFor(() => expect(screen.getByRole("button", { name: "添加渠道" })).toBeEnabled());
  fireEvent.click(screen.getByRole("button", { name: "添加渠道" }));
}

import { NotifyChannelsPanel } from "../NotifyChannelsPanel";

beforeEach(() => { backend.reset(); vi.clearAllMocks(); });

describe("NotifyChannelsPanel", () => {

  it("无渠道时显示空态", async () => {
    render(<NotifyChannelsPanel />);
    await openAdvanced();
    expect(await screen.findByText("尚未添加外部推送，日常桌面提醒无需配置这里。")).toBeInTheDocument();
    expect(backend.load).toHaveBeenCalled();
  });

  it("添加渠道后持久化并显示在列表", async () => {
    render(<NotifyChannelsPanel onToast={vi.fn()} />);
    await openAdd();
    await waitFor(() => expect(screen.getByRole("combobox")).toBeEnabled());
    const inputs = screen.getAllByRole("textbox");
    fireEvent.change(inputs[0], { target: { value: "我的Slack" } });
    fireEvent.change(inputs[1], { target: { value: "https://hooks.slack.com/x" } });
    fireEvent.click(screen.getByText("+ 添加"));
    await waitFor(() => expect(screen.getByText("我的Slack")).toBeInTheDocument());
    expect(backend.save).toHaveBeenCalledWith(expect.objectContaining({
      label: "我的Slack",
      endpoint: "https://hooks.slack.com/x",
      enabled: true,
    }));
  });

  it("移除渠道", async () => {
    backend.seed([{ id: "test", label: "Test", kind: "generic-webhook", enabled: true }]);
    render(<NotifyChannelsPanel onToast={vi.fn()} />);
    await openAdvanced();
    await screen.findByText("Test");
    fireEvent.click(screen.getByTitle("移除"));
    const dialog = screen.getByRole("alertdialog", { name: "移除通知渠道？" });
    expect(backend.remove).not.toHaveBeenCalled();
    fireEvent.click(within(dialog).getByRole("button", { name: "移除" }));
    await waitFor(() => expect(screen.queryByText("Test")).toBeNull());
    expect(backend.remove).toHaveBeenCalledWith("test");
  });

  it("移除失败时保留确认框并可重试", async () => {
    backend.seed([{ id: "test", label: "Test", kind: "generic-webhook", enabled: true }]);
    backend.remove.mockRejectedValueOnce(new Error("配置文件被占用"));
    render(<NotifyChannelsPanel onToast={vi.fn()} />);
    await openAdvanced();
    fireEvent.click(await screen.findByTitle("移除"));
    fireEvent.click(screen.getByRole("button", { name: "移除" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("配置文件被占用");
    expect(screen.getByText("Test")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "移除" }));
    await waitFor(() => expect(screen.queryByRole("alertdialog")).toBeNull());
    expect(backend.remove).toHaveBeenCalledTimes(2);
  });

  it("切换启用/禁用", async () => {
    backend.seed([{ id: "test", label: "Test", kind: "generic-webhook", enabled: true }]);
    render(<NotifyChannelsPanel />);
    await openAdvanced();
    fireEvent.click(await screen.findByTitle("禁用"));
    await waitFor(() => expect(screen.getByTitle("启用")).toBeInTheDocument());
    expect(backend.setEnabled).toHaveBeenCalledWith("test", false);
  });

  it("测试发送调用后端测试命令", async () => {
    backend.seed([{
      id: "test",
      label: "Webhook",
      kind: "generic-webhook",
      endpoint: "http://localhost:9999/hook",
      enabled: true,
    }]);
    const onToast = vi.fn();
    render(<NotifyChannelsPanel onToast={onToast} />);
    await openAdvanced();
    fireEvent.click(await screen.findByText("测试"));
    await waitFor(() => expect(onToast).toHaveBeenCalledWith("测试通知已向渠道提交"));
    expect(backend.test).toHaveBeenCalledWith("test");
  });

  it("编辑渠道名称时保留后端 Webhook 凭据，不把原始地址渲染在列表", async () => {
    backend.seed([{
      id: "test", label: "旧名称", kind: "slack-webhook",
      endpoint: "https://hooks.example.test/private-token", enabled: true,
    }]);
    render(<NotifyChannelsPanel />);
    await openAdvanced();
    await screen.findByText("旧名称");
    expect(screen.queryByText(/private-token/)).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "编辑" }));
    expect(screen.getByRole("textbox", { name: "Webhook URL" })).toHaveValue("");
    fireEvent.change(screen.getByRole("textbox", { name: "通知渠道显示名" }), { target: { value: "新名称" } });
    fireEvent.click(screen.getByRole("button", { name: "保存修改" }));
    await screen.findByText("新名称");
    expect(backend.save).toHaveBeenCalledWith(expect.objectContaining({
      id: "test", label: "新名称", endpoint: undefined,
    }));
    expect(screen.queryByText(/private-token/)).toBeNull();
  });

  it("保存成功但列表刷新失败时明确提示重试，不误报列表已更新", async () => {
    const onToast = vi.fn();
    render(<NotifyChannelsPanel onToast={onToast} />);
    await openAdd();
    backend.load.mockRejectedValueOnce(new Error("配置暂时不可读"));
    fireEvent.change(screen.getByRole("textbox", { name: "Webhook URL" }), {
      target: { value: "https://hooks.example.test/new" },
    });
    fireEvent.click(screen.getByRole("button", { name: "+ 添加" }));
    await waitFor(() => expect(onToast).toHaveBeenCalledWith(expect.stringContaining("已保存，列表暂未更新")));
    expect(screen.getByRole("alert")).toHaveTextContent("配置暂时不可读");
    fireEvent.click(screen.getByRole("button", { name: "重试" }));
    await waitFor(() => expect(screen.getByText("1 个已配置")).toBeInTheDocument());
    expect(screen.queryByRole("alert")).toBeNull();
  });
});


it("桌面提醒默认可测并携带任务目标，系统授权帮助按需展开", async () => {
  render(<NotifyChannelsPanel sessionId="active-task" />);
  expect(await screen.findByText(/已开启 · 默认设置/)).toBeInTheDocument();
  expect(screen.getByRole("switch", {name:"桌面通知"})).toBeChecked();
  fireEvent.click(screen.getByRole("button", {name:"发送测试通知"}));
  expect(await screen.findByRole("status")).toHaveTextContent("已向系统提交通知");
  expect(backend.desktopTest).toHaveBeenCalledWith("active-task");
  expect(screen.queryByText(/系统通知权限：未知/)).toBeNull();
  fireEvent.click(screen.getByText("收不到桌面通知？"));
  expect(screen.getByText(/应用目前无法读取系统授权状态/)).toBeVisible();
});

it("显式关闭保留记录，桌面配置不进入高级渠道表单", async () => {
  backend.seed([{id:"off",label:"我的桌面",kind:"desktop",enabled:false}]);
  render(<NotifyChannelsPanel />);
  expect(await screen.findByText(/已关闭/)).toBeInTheDocument();
  expect(screen.getByRole("button", {name:"发送测试通知"})).toBeDisabled();
  expect(screen.queryByRole("button", {name:"移除 我的桌面"})).toBeNull();
  expect(screen.queryByRole("option", {name:"桌面通知"})).toBeNull();
  fireEvent.click(screen.getByRole("switch", {name:"桌面通知"}));
  await waitFor(() => expect(backend.desktopToggle).toHaveBeenCalledWith(true));
  expect(await screen.findByText(/^已开启$/)).toBeInTheDocument();
});

it("组织策略限制和投递失败不会显示为已授权或发送成功", async () => {
  backend.desktopStatus.mockResolvedValueOnce({enabled:false, source:"policy",permission:"unknown",policyError:"组织禁止通知"} as never);
  render(<NotifyChannelsPanel />);
  expect(await screen.findByText(/组织策略已限制桌面通知/)).toBeInTheDocument();
  expect(screen.getByRole("switch", {name:"桌面通知"})).toBeDisabled();
  expect(screen.getByRole("button", {name:"发送测试通知"})).toBeDisabled();
});

it("桌面提交错误和开关保存失败均可见，并保留先前状态", async () => {
  const onToast = vi.fn();
  backend.desktopTest.mockRejectedValueOnce(new Error("系统拒绝通知"));
  backend.desktopToggle.mockRejectedValueOnce(new Error("配置不可写"));
  render(<NotifyChannelsPanel onToast={onToast} />);
  await screen.findByText(/已开启 · 默认设置/);
  fireEvent.click(screen.getByRole("button", {name:"发送测试通知"}));
  expect(await screen.findByRole("status")).toHaveTextContent("系统拒绝通知");
  fireEvent.click(screen.getByRole("switch", {name:"桌面通知"}));
  await waitFor(() => expect(onToast).toHaveBeenCalledWith(expect.stringContaining("配置不可写")));
  expect(screen.getByRole("switch", {name:"桌面通知"})).toBeChecked();
});


it("默认只显示日常提醒与隐私设置，添加表单按需打开并可取消", async () => {
  render(<NotifyChannelsPanel />);
  await screen.findByText(/已开启 · 默认设置/);
  expect(screen.getByRole("switch", { name: "在通知中显示任务标题" })).not.toBeChecked();
  expect(screen.getByRole("button", { name: /推送到其他应用/ })).toHaveAttribute("aria-expanded", "false");
  expect(screen.queryByRole("textbox")).toBeNull();
  await openAdd();
  expect(screen.getByRole("form", { name: "添加通知渠道" })).toBeInTheDocument();
  fireEvent.click(screen.getByRole("button", { name: "取消" }));
  expect(screen.queryByRole("textbox")).toBeNull();
  expect(backend.save).not.toHaveBeenCalled();
});

it("外部渠道加载失败不阻断桌面提醒与隐私设置", async () => {
  backend.load.mockRejectedValueOnce(new Error("渠道配置暂时不可读"));
  render(<NotifyChannelsPanel />);
  await screen.findByText(/已开启 · 默认设置/);
  expect(screen.getByRole("switch", { name: "桌面通知" })).toBeEnabled();
  expect(screen.getByRole("button", { name: "发送测试通知" })).toBeEnabled();
  await openAdvanced();
  expect(screen.getByRole("alert")).toHaveTextContent("渠道配置暂时不可读");
  fireEvent.click(screen.getByRole("button", { name: "发送测试通知" }));
  expect(await screen.findByRole("status")).toHaveTextContent("已向系统提交通知");
  fireEvent.click(screen.getByRole("button", { name: "重试" }));
  await waitFor(() => expect(screen.queryByRole("alert")).toBeNull());
});

it("桌面状态失败只禁用桌面设置，重试后恢复", async () => {
  backend.desktopStatus.mockRejectedValueOnce(new Error("暂时无法读取"));
  render(<NotifyChannelsPanel />);
  expect(await screen.findByRole("alert")).toHaveTextContent("桌面提醒状态不可用");
  expect(screen.getByRole("switch", { name: "桌面通知" })).toBeDisabled();
  await openAdd();
  expect(screen.getByRole("textbox", { name: "Webhook URL" })).toBeEnabled();
  fireEvent.click(screen.getByRole("button", { name: "重试读取桌面提醒" }));
  await waitFor(() => expect(screen.getByRole("switch", { name: "桌面通知" })).toBeEnabled());
});

it("已停用的旧邮件渠道只允许移除，不提供无法生效的启用和测试", async () => {
  backend.seed([{ id: "legacy", label: "旧邮箱", kind: "email", enabled: true }]);
  render(<NotifyChannelsPanel />);
  await openAdvanced();
  await screen.findByText("旧邮箱");
  expect(screen.queryByRole("switch", { name: /旧邮箱/ })).toBeNull();
  expect(screen.queryByRole("button", { name: "测试" })).toBeNull();
  expect(screen.queryByRole("button", { name: "编辑" })).toBeNull();
  expect(screen.getByRole("button", { name: "移除 旧邮箱" })).toBeEnabled();
});

it("隐私偏好保存失败时保留旧值，并可重新读取后再保存", async () => {
  vi.mocked(desktopNotificationPreviewSave).mockRejectedValueOnce(new Error("文件只读"));
  render(<NotifyChannelsPanel />);
  const preview = screen.getByRole("switch", { name: "在通知中显示任务标题" });
  await waitFor(() => expect(preview).toBeEnabled());
  fireEvent.click(preview);
  expect(await screen.findByRole("alert")).toHaveTextContent("文件只读");
  expect(preview).not.toBeChecked();
  fireEvent.click(screen.getByRole("button", { name: "重试读取通知内容" }));
  await waitFor(() => expect(preview).toBeEnabled());
  fireEvent.click(preview);
  await waitFor(() => expect(preview).toBeChecked());
  expect(desktopPreferencesGet).toHaveBeenCalledTimes(2);
});


it("渠道取消需确认，保存失败在未提供 Toast 时也可见且保留输入", async () => {
  render(<NotifyChannelsPanel />);
  await openAdd();
  const endpoint = screen.getByRole("textbox", { name: "Webhook URL" });
  fireEvent.change(endpoint, { target: { value: "https://hooks.example.test/draft" } });
  fireEvent.click(screen.getByRole("button", { name: "取消" }));
  const dialog = await screen.findByRole("alertdialog", { name: "舍弃通知渠道的修改？" });
  fireEvent.click(within(dialog).getByRole("button", { name: "取消" }));
  expect(endpoint).toHaveValue("https://hooks.example.test/draft");
  backend.save.mockRejectedValueOnce(new Error("配置只读"));
  fireEvent.click(screen.getByRole("button", { name: "+ 添加" }));
  expect(await screen.findByRole("alert")).toHaveTextContent("配置只读");
  expect(endpoint).toHaveValue("https://hooks.example.test/draft");
});
