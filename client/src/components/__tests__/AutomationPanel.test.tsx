import { buildDraft } from "../automation/schedule-utils";
import { describe, it, expect, vi, beforeEach } from "vitest";
import { act, render, screen, fireEvent, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { AutomationPanel } from "../AutomationPanel";
import { automationEditorDirty, useAutomationEditorStore } from "@/stores/automation-editor-store";
import { AutomationPermissionConfirmDialog } from "../automation/AutomationPermissionConfirmDialog";
import type { AutomationSnapshot } from "@/lib/types";

// ---- mock Tauri IPC 层 ----
const emptySnapshot: AutomationSnapshot = { automations: [], records: [] };
let snapshot: AutomationSnapshot = emptySnapshot;

vi.mock("@/lib/agent-client", () => ({
  automationsSnapshot: vi.fn(async () => snapshot),
  automationsSave: vi.fn(async (a: unknown) => a),
  automationsDelete: vi.fn(async () => {}),
  automationsSetStatus: vi.fn(async () => {}),
  automationsRun: vi.fn(async () => "record-id"),
  automationRecordsArchive: vi.fn(async () => {}),
  automationRecordsDelete: vi.fn(async () => {}),
  agentListWorkspaces: vi.fn(async () => []),
  filesystemPickDirectory: vi.fn(async () => null),
  agentAuthStatus: vi.fn(async () => ({
    ready: true,
    providers: [],
    runtimeReady: true,
    synchronized: true,
    runtimeModels: [],
  })),
  providersList: vi.fn(async () => ({ providers: [], models: [] })),
  skillsList: vi.fn(async () => []),
  agentsList: vi.fn(async () => []),
  mcpList: vi.fn(async () => []),
  flattenModels: vi.fn((catalog: { models?: unknown[] }) => catalog.models ?? []),
  filterModelsByRuntimeCatalog: vi.fn((options: unknown[]) => options),
}));

import {
  agentListWorkspaces,
  filesystemPickDirectory,
  agentAuthStatus,
  agentsList,
  automationsDelete,
  automationsRun,
  automationsSave,
  automationsSnapshot,
  mcpList,
  providersList,
  skillsList,
} from "@/lib/agent-client";

beforeEach(() => {
  useAutomationEditorStore.getState().close();
  snapshot = emptySnapshot;
  vi.mocked(automationsSnapshot).mockReset().mockImplementation(async () => snapshot);
  vi.mocked(agentListWorkspaces).mockReset().mockResolvedValue([]);
  vi.mocked(filesystemPickDirectory).mockReset().mockResolvedValue(null);
  vi.mocked(agentAuthStatus).mockReset().mockResolvedValue({
    ready: true,
    providers: [],
    runtimeReady: true,
    synchronized: true,
    runtimeModels: [],
  });
  vi.mocked(automationsSave).mockReset().mockImplementation(async (automation) => automation);
  vi.mocked(automationsDelete).mockReset().mockResolvedValue(undefined);
  vi.mocked(automationsRun).mockReset().mockResolvedValue("record-id");
  vi.mocked(providersList).mockReset().mockResolvedValue({ providers: [], models: [] });
  vi.mocked(skillsList).mockReset().mockResolvedValue([]);
  vi.mocked(agentsList).mockReset().mockResolvedValue([]);
  vi.mocked(mcpList).mockReset().mockResolvedValue([]);
});

function beginRunnableEditor() {
  const draft = { ...buildDraft(undefined, "/tmp"), id: "job-a", name: "原任务", prompt: "读取报告", modelId: "ready-model" };
  const automation = { ...draft, createdAt: "2026-10-17T00:00:00Z" };
  snapshot = { automations: [automation], records: [] };
  vi.mocked(providersList).mockResolvedValue({ providers: [], models: [{ id: "ready-model", label: "可用模型" }] } as never);
  vi.mocked(agentAuthStatus).mockResolvedValue({ ready: true, providers: [], runtimeReady: true, synchronized: true, runtimeModels: ["ready-model"], defaultModelId: "ready-model" });
  useAutomationEditorStore.getState().begin(draft, automation);
  return automation;
}

describe("自动化编辑流程与通知导航", () => {
  const ready = () => waitFor(() => expect(screen.queryByText("正在加载工作空间、模型与运行时能力…")).toBeNull());

  it.each(["保存", "测试运行"])("%s 不会重新创建已删除的任务", async (action) => {
    beginRunnableEditor(); snapshot = emptySnapshot;
    const toast = vi.fn(); render(<AutomationPanel onToast={toast} />); await ready();
    fireEvent.click(screen.getByRole("button", { name: action }));
    expect(toast).toHaveBeenCalledWith(expect.stringContaining("已被移除"));
    expect(automationsSave).not.toHaveBeenCalled(); expect(automationsRun).not.toHaveBeenCalled();
    expect(screen.getByDisplayValue("原任务")).toBeInTheDocument();
  });

  it("删除中阻止重复写操作，跨页返回后仍可看见删除进度", async () => {
    beginRunnableEditor(); let resolveDelete!: () => void;
    vi.mocked(automationsDelete).mockReturnValueOnce(new Promise(resolve => { resolveDelete = resolve; }));
    const first = render(<AutomationPanel />); await ready();
    fireEvent.click(screen.getByRole("button", { name: "删除" }));
    fireEvent.click(screen.getByRole("button", { name: "删除自动化任务" }));
    expect(screen.getByRole("button", { name: "保存" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "测试运行" })).toBeDisabled();
    first.unmount(); render(<AutomationPanel />);
    expect(screen.getByText("正在删除自动化任务…")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "取消" })).toBeDisabled();
    await act(async () => resolveDelete());
    expect(useAutomationEditorStore.getState().draft).toBeNull();
    expect(automationsDelete).toHaveBeenCalledTimes(1);
  });

  it("旧页面迟到的删除响应不能清空另一个编辑流程", async () => {
    beginRunnableEditor(); let resolveDelete!: () => void;
    vi.mocked(automationsDelete).mockReturnValueOnce(new Promise(resolve => { resolveDelete = resolve; }));
    const first = render(<AutomationPanel />);
    fireEvent.click(screen.getByRole("button", { name: "删除" }));
    fireEvent.click(screen.getByRole("button", { name: "删除自动化任务" }));
    first.unmount();
    useAutomationEditorStore.getState().close();
    useAutomationEditorStore.getState().begin({ ...buildDraft(), name: "新的草稿" });
    render(<AutomationPanel />);
    await act(async () => resolveDelete());
    expect(screen.getByDisplayValue("新的草稿")).toBeInTheDocument();
  });

  it("删除失败保留草稿、恢复按钮，并允许重试", async () => {
    beginRunnableEditor(); vi.mocked(automationsDelete).mockRejectedValueOnce(new Error("磁盘忙"));
    const toast = vi.fn(); render(<AutomationPanel onToast={toast} />);
    fireEvent.change(screen.getByDisplayValue("原任务"), { target: { value: "删除失败仍保留" } });
    fireEvent.click(screen.getByRole("button", { name: "删除" }));
    fireEvent.click(screen.getByRole("button", { name: "删除自动化任务" }));
    await waitFor(() => expect(toast).toHaveBeenCalledWith(expect.stringContaining("磁盘忙")));
    expect(screen.getByDisplayValue("删除失败仍保留")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "删除" })).toBeEnabled();
    expect(automationEditorDirty()).toBe(true);
  });

  it.each([false, true])("批量删除正在编辑的任务时，失败=%s 的草稿处理与结果一致", async (fails) => {
    beginRunnableEditor();
    vi.mocked(automationsDelete).mockImplementationOnce(async () => {
      if (fails) throw new Error("磁盘忙");
      snapshot = emptySnapshot;
    });
    const toast = vi.fn(); render(<AutomationPanel onToast={toast} />); await ready();
    expect(screen.getByRole("textbox", { name: "任务要求" })).toBeInTheDocument();
    fireEvent.change(screen.getByRole("textbox", { name: "名称" }), { target: { value: "批量操作期间保留" } });
    fireEvent.click(screen.getByRole("button", { name: "自动化" }));
    fireEvent.click(screen.getByRole("button", { name: "批量管理" }));
    fireEvent.click(screen.getByRole("button", { name: "全选" }));
    fireEvent.click(screen.getByRole("button", { name: "删除" }));
    const dialog = await screen.findByRole("dialog");
    expect(dialog).toHaveTextContent("任务草稿也会被舍弃");
    fireEvent.click(within(dialog).getByRole("button", { name: "删除" }));
    if (fails) await waitFor(() => expect(toast).toHaveBeenCalledWith("已删除 0 个任务，1 个未能删除，请重试"));
    else {
      await waitFor(() => expect(useAutomationEditorStore.getState().draft).toBeNull());
      expect(toast).not.toHaveBeenCalled();
    }
    expect(useAutomationEditorStore.getState().operation).toBeNull();
    if (fails) {
      fireEvent.click(screen.getByRole("button", { name: "继续编辑" }));
      expect(screen.getByDisplayValue("批量操作期间保留")).toBeInTheDocument();
    } else expect(useAutomationEditorStore.getState().draft).toBeNull();
  });

  it.each([false, true])("测试保存成功后同步基线，即使运行失败=%s 也不会谎称修改未保存", async (runFails) => {
    beginRunnableEditor();
    if (runFails) vi.mocked(automationsRun).mockRejectedValueOnce(new Error("运行时断线"));
    vi.mocked(automationsSave).mockImplementationOnce(async a => ({ ...a, name: a.name.trim() }));
    const toast = vi.fn(); render(<AutomationPanel onToast={toast} />); await ready();
    fireEvent.change(screen.getByDisplayValue("原任务"), { target: { value: " 已保存的新名称 " } });
    fireEvent.click(screen.getByRole("button", { name: "测试运行" }));
    await waitFor(() => expect(useAutomationEditorStore.getState().operation).toBeNull());
    expect(automationsRun).toHaveBeenCalledWith("job-a");
    expect(screen.getByDisplayValue("已保存的新名称")).toBeInTheDocument();
    expect(automationEditorDirty()).toBe(false);
    if (runFails) expect(toast).toHaveBeenCalledWith(expect.stringContaining("修改已保存，但触发测试运行失败"));
    fireEvent.click(screen.getByRole("button", { name: "取消" }));
    expect(screen.queryByRole("alertdialog")).toBeNull();
    expect(useAutomationEditorStore.getState().draft).toBeNull();
  });

  it("测试保存被后端拒绝时保留输入，不能触发运行", async () => {
    beginRunnableEditor(); vi.mocked(automationsSave).mockRejectedValueOnce(new Error("原自动化任务已被移除"));
    const toast = vi.fn(); render(<AutomationPanel onToast={toast} />); await ready();
    fireEvent.change(screen.getByDisplayValue("原任务"), { target: { value: "保留新输入" } });
    fireEvent.click(screen.getByRole("button", { name: "测试运行" }));
    await waitFor(() => expect(toast).toHaveBeenCalledWith(expect.stringContaining("已被移除")));
    expect(automationsRun).not.toHaveBeenCalled(); expect(automationEditorDirty()).toBe(true);
    expect(screen.getByDisplayValue("保留新输入")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "保存" })).toBeEnabled();
  });

  it("重复测试点击只保存一次，旧页面保存响应不会改写新的编辑流程或继续运行", async () => {
    beginRunnableEditor(); let resolveSave!: (a: Awaited<ReturnType<typeof automationsSave>>) => void;
    vi.mocked(automationsSave).mockReturnValueOnce(new Promise(resolve => { resolveSave = resolve; }));
    const first = render(<AutomationPanel />); await ready();
    const test = screen.getByRole("button", { name: "测试运行" });
    act(() => { fireEvent.click(test); fireEvent.click(test); });
    expect(automationsSave).toHaveBeenCalledTimes(1);
    const submitted = vi.mocked(automationsSave).mock.calls[0][0]; first.unmount();
    useAutomationEditorStore.getState().close();
    useAutomationEditorStore.getState().begin({ ...buildDraft(), name: "新的编辑流程" });
    render(<AutomationPanel />);
    await act(async () => resolveSave(submitted));
    expect(screen.getByDisplayValue("新的编辑流程")).toBeInTheDocument();
    expect(automationsRun).not.toHaveBeenCalled();
    expect(useAutomationEditorStore.getState().isCreating).toBe(true);
  });

  it("已删除任务可显式转为新任务草稿，保留全部输入且仅在保存后创建", async () => {
    beginRunnableEditor(); snapshot = emptySnapshot;
    render(<AutomationPanel />); await ready();
    fireEvent.change(screen.getByDisplayValue("原任务"), { target: { value: "恢复的重要内容" } });
    fireEvent.click(await screen.findByRole("button", { name: "转为新任务草稿" }));
    expect(screen.getByDisplayValue("恢复的重要内容")).toBeInTheDocument();
    expect(useAutomationEditorStore.getState().draft?.prompt).toBe("读取报告");
    expect(automationEditorDirty()).toBe(true);
    expect(automationsSave).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "保存" }));
    await waitFor(() => expect(automationsSave).toHaveBeenCalledWith(expect.objectContaining({ id: "", name: "恢复的重要内容" })));
  });

  it("保留草稿期间暂停原任务，继续保存不会意外重新启用任务", async () => {
    beginRunnableEditor(); render(<AutomationPanel />); await ready();
    fireEvent.change(screen.getByDisplayValue("原任务"), { target: { value: "保留修改" } });
    fireEvent.click(screen.getByRole("button", { name: "自动化" }));
    fireEvent.click(screen.getByRole("button", { name: "打开「原任务」更多操作" }));
    fireEvent.click(screen.getByRole("button", { name: "暂停" }));
    await waitFor(() => expect(useAutomationEditorStore.getState().operation).toBeNull());
    fireEvent.click(screen.getByRole("button", { name: "继续编辑" }));
    expect(automationEditorDirty()).toBe(true);
    fireEvent.click(screen.getByRole("button", { name: "保存" }));
    await waitFor(() => expect(automationsSave).toHaveBeenCalledWith(expect.objectContaining({ status: "PAUSED", name: "保留修改" })));
  });

  it("通知打开目标记录且保留草稿，用户可恢复编辑，重复通知仍可重新导航", async () => {
    beginRunnableEditor(); useAutomationEditorStore.getState().setDraft(d => d && ({ ...d, name: "保留的输入" }));
    const { rerender } = render(<AutomationPanel notificationAutomationId="job-b" notificationAutomationSequence={1} />);
    expect(await screen.findByRole("tab", { name: "运行记录" })).toHaveAttribute("aria-selected", "true");
    expect(screen.getByRole("status", { name: "保留的自动化草稿" })).toHaveTextContent("保留的输入");
    expect(screen.getByText(/正在查看“关联自动化”/)).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "继续编辑" }));
    expect(screen.getByDisplayValue("保留的输入")).toBeInTheDocument();
    rerender(<AutomationPanel notificationAutomationId="job-b" notificationAutomationSequence={2} />);
    expect(screen.getByRole("tab", { name: "运行记录" })).toHaveAttribute("aria-selected", "true");
    expect(automationsSave).not.toHaveBeenCalled();
  });

  it("返回列表保留草稿，创建另一任务前明确确认，取消不会丢失输入", async () => {
    beginRunnableEditor(); render(<AutomationPanel />); await ready();
    fireEvent.change(screen.getByDisplayValue("原任务"), { target: { value: "尚未保存" } });
    fireEvent.click(screen.getByRole("button", { name: "自动化" }));
    expect(screen.getByRole("status", { name: "保留的自动化草稿" })).toHaveTextContent("尚未保存");
    fireEvent.click(screen.getByRole("button", { name: "添加自动化" }));
    fireEvent.click(within(await screen.findByRole("alertdialog")).getByRole("button", { name: "保留草稿" }));
    expect(useAutomationEditorStore.getState().draft?.name).toBe("尚未保存");
    fireEvent.click(screen.getByRole("button", { name: "添加自动化" }));
    fireEvent.click(within(await screen.findByRole("alertdialog")).getByRole("button", { name: "舍弃并继续" }));
    await waitFor(() => expect(useAutomationEditorStore.getState().isCreating).toBe(true));
    expect(useAutomationEditorStore.getState().draft?.name).toBe("");
  });
});

describe("AutomationPanel（截图 1/3 空态）", () => {
  it("恢复编辑时原任务已移除则保留草稿并拒绝重新创建旧任务", async () => {
    const draft = { ...buildDraft(), id: "removed-job", name: "保留内容" };
    useAutomationEditorStore.getState().begin(draft, { ...draft, createdAt: "2026-10-17T00:00:00Z" });
    const toast = vi.fn(); render(<AutomationPanel onToast={toast} />);
    await waitFor(() => expect(automationsSnapshot).toHaveBeenCalled());
    fireEvent.click(await screen.findByRole("button", { name: "保存" }));
    expect(toast).toHaveBeenCalledWith(expect.stringContaining("已被移除"));
    expect(screen.getByDisplayValue("保留内容")).toBeInTheDocument();
    expect(automationsSave).not.toHaveBeenCalled();
  });

  it("离开页面再返回时保留尚未提交的自动化输入，不创建实际任务", async () => {
    const first = render(<AutomationPanel />);
    fireEvent.click(await screen.findByText("+ 添加自动化"));
    fireEvent.change(document.querySelector(".atm-modal-input")!, { target: { value: "跨页保留的草稿" } });
    first.unmount();
    render(<AutomationPanel />);
    expect(await screen.findByDisplayValue("跨页保留的草稿")).toBeInTheDocument();
    expect(automationsSave).not.toHaveBeenCalled();
  });

  it("首次加载被后台快照刷新取代时结束加载，迟到的响应不能覆盖新快照", async () => {
    let resolveInitial!: (value: AutomationSnapshot) => void;
    vi.mocked(automationsSnapshot)
      .mockReturnValueOnce(new Promise((resolve) => { resolveInitial = resolve; }))
      .mockResolvedValueOnce(emptySnapshot);
    const { rerender } = render(<AutomationPanel refreshSignal={0} />);
    rerender(<AutomationPanel refreshSignal={1} />);
    expect(await screen.findByText("开启你的第一个自动化任务吧")).toBeInTheDocument();
    await act(async () => resolveInitial({ automations: [], records: [] }));
    expect(screen.getByText("开启你的第一个自动化任务吧")).toBeInTheDocument();
  });

  it("首次加载被失败的后台刷新取代时显示可重试错误，不能一直加载", async () => {
    let resolveInitial!: (value: AutomationSnapshot) => void;
    vi.mocked(automationsSnapshot)
      .mockReturnValueOnce(new Promise((resolve) => { resolveInitial = resolve; }))
      .mockRejectedValueOnce(new Error("后台刷新断线"));
    const { rerender } = render(<AutomationPanel refreshSignal={0} />);
    rerender(<AutomationPanel refreshSignal={1} />);
    expect(await screen.findByRole("alert")).toHaveTextContent("后台刷新断线");
    fireEvent.click(screen.getByRole("button", { name: "重试" }));
    expect(await screen.findByText("开启你的第一个自动化任务吧")).toBeInTheDocument();
    await act(async () => resolveInitial(emptySnapshot));
  });

  it("渲染 定时任务/运行记录 页签", async () => {
    render(<AutomationPanel />);
    expect(screen.getByText("定时任务")).toBeInTheDocument();
    expect(screen.getByText("运行记录")).toBeInTheDocument();
    await waitFor(() => expect(screen.getByText("开启你的第一个自动化任务吧")).toBeInTheDocument());
  });

  it("定时任务空态：hero 文案 + 添加按钮 + 12 个模板", async () => {
    render(<AutomationPanel />);
    await waitFor(() => expect(screen.getByText("开启你的第一个自动化任务吧")).toBeInTheDocument());
    expect(screen.getByText("+ 添加自动化")).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "自动化任务模板" })).toBeInTheDocument();
    expect(screen.getByText("每日 AI 新闻推送")).toBeInTheDocument();
    expect(screen.getByText("可爱萌宠手机壁纸")).toBeInTheDocument();
  });

  it("运行记录空态：暂无运行记录", async () => {
    render(<AutomationPanel />);
    fireEvent.click(screen.getByText("运行记录"));
    await waitFor(() => expect(screen.getByText("暂无运行记录")).toBeInTheDocument());
  });

  it("取消自动化编辑前保护未保存的输入，继续编辑保留内容，确认后退出", async () => {
    render(<AutomationPanel />);
    fireEvent.click(await screen.findByText("+ 添加自动化"));
    fireEvent.change(document.querySelector(".atm-modal-input")!, { target: { value: "不能丢失的定时任务" } });
    fireEvent.click(screen.getByRole("button", { name: "取消" }));
    const dialog = await screen.findByRole("alertdialog", { name: "舍弃未保存的修改？" });
    fireEvent.click(within(dialog).getByRole("button", { name: "继续编辑" }));
    expect(screen.getByDisplayValue("不能丢失的定时任务")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "取消" }));
    fireEvent.click(within(await screen.findByRole("alertdialog")).getByRole("button", { name: "舍弃修改" }));
    expect(await screen.findByText("开启你的第一个自动化任务吧")).toBeInTheDocument();
  });

  it("自动化工具条下拉支持 Escape 关闭并把焦点还给触发按钮", async () => {
    render(<AutomationPanel />);
    fireEvent.click(await screen.findByText("+ 添加自动化"));
    const trigger = screen.getByRole("button", { name: /^技能/ });
    fireEvent.click(trigger);
    expect(screen.getByText("暂无可用技能")).toBeInTheDocument();
    fireEvent.keyDown(document, { key: "Escape" });
    expect(screen.queryByText("暂无可用技能")).toBeNull();
    expect(trigger).toHaveFocus();
  });

  it("首次加载失败显示持久错误和重试，不伪装成空列表", async () => {
    vi.mocked(automationsSnapshot).mockRejectedValueOnce(new Error("快照损坏"));
    render(<AutomationPanel />);

    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent("自动化数据加载失败：快照损坏");
    expect(screen.queryByText("开启你的第一个自动化任务吧")).not.toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "重试" }));
    expect(await screen.findByText("开启你的第一个自动化任务吧")).toBeInTheDocument();
  });

  it("点击「+ 添加自动化」进入表单（截图 2 字段）", async () => {
    render(<AutomationPanel />);
    await waitFor(() => screen.getByText("+ 添加自动化"));
    fireEvent.click(screen.getByText("+ 添加自动化"));
    await waitFor(() => expect(screen.getByText("添加自动化任务")).toBeInTheDocument());
    expect(screen.getByText("名称")).toBeInTheDocument();
    expect(screen.getByText("工作目录", { selector: "label" })).toBeInTheDocument();
    expect(screen.getByText("任务要求")).toBeInTheDocument();
    expect(screen.getByText("执行频率")).toBeInTheDocument();
    expect(screen.getByText("周期")).toBeInTheDocument();
    expect(screen.getByText("按间隔")).toBeInTheDocument();
    expect(screen.getByText("单次")).toBeInTheDocument();
    expect(screen.getByText(/生效日期区间/)).toBeInTheDocument();
    expect(screen.getByText(/完成后推送到通知渠道/)).toBeInTheDocument();
    expect(screen.getByText("取消")).toBeInTheDocument();
    expect(screen.getByText("保存")).toBeInTheDocument();
    // 提示词工具条 chips
    expect(screen.getByText(/Auto/)).toBeInTheDocument();
    expect(screen.getByText(/技能/)).toBeInTheDocument();
    expect(screen.getByText("召唤专家")).toBeInTheDocument();
    expect(screen.getByText("默认权限")).toBeInTheDocument();
  });

  it("工作空间可输入路径并通过系统目录选择授权新路径", async () => {
    vi.mocked(agentListWorkspaces).mockResolvedValue([{ cwd: "/recent", sessionCount: 2 }]);
    vi.mocked(filesystemPickDirectory).mockResolvedValue("/new/custom");
    vi.mocked(providersList).mockResolvedValue({ providers: [], models: [{ id: "ready-model", label: "可用模型" }] } as never);
    vi.mocked(agentAuthStatus).mockResolvedValue({
      ready: true, providers: ["ready-model"], runtimeReady: true,
      synchronized: true, runtimeModels: ["ready-model"], defaultModelId: "ready-model",
    });
    render(<AutomationPanel cwd="/recent" />);
    fireEvent.click(await screen.findByText("+ 添加自动化"));

    const input = document.querySelector("#automation-workspace-path") as HTMLInputElement;
    expect(input.value).toBe("/recent");
    fireEvent.focus(input);
    expect(screen.queryByRole("button", { name: /recent/ })).not.toBeInTheDocument();

    fireEvent.change(input, { target: { value: "/new/custom" } });
    expect(input.value).toBe("/new/custom");
    fireEvent.click(screen.getByRole("button", { name: "选择文件夹" }));
    await waitFor(() => expect(filesystemPickDirectory).toHaveBeenCalledWith("/new/custom"));
    expect(input.value).toBe("/new/custom");

    fireEvent.change(document.querySelector(".atm-modal-input") as HTMLInputElement, { target: { value: "自定义目录任务" } });
    fireEvent.change(document.querySelector(".atm-prompt-textarea") as HTMLTextAreaElement, { target: { value: "运行任务" } });
    fireEvent.click(screen.getByRole("button", { name: "保存" }));
    await waitFor(() => expect(automationsSave).toHaveBeenCalledTimes(1));
    expect(vi.mocked(automationsSave).mock.calls[0][0].cwds).toBe("/new/custom");
  });

  it("引用目录分项失败时保留成功数据并可重试，不伪装空状态", async () => {
    vi.mocked(providersList).mockRejectedValueOnce(new Error("模型目录离线"));
    vi.mocked(skillsList).mockResolvedValueOnce([
      { name: "review", displayName: "代码审查", enabled: true },
    ]);
    render(<AutomationPanel />);
    fireEvent.click(await screen.findByText("+ 添加自动化"));

    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent("模型：模型目录离线");
    expect(alert).toHaveTextContent("已保留其他可用数据");
    fireEvent.click(screen.getByRole("button", { name: /^技能/ }));
    expect(screen.getByRole("button", { name: "代码审查" })).toBeInTheDocument();

    fireEvent.click(within(alert).getByRole("button", { name: "重试" }));
    await waitFor(() => expect(screen.queryByRole("alert")).not.toBeInTheDocument());
  });

  it("显式选择的模型会写入保存请求，不会回退到 Auto", async () => {
    vi.mocked(providersList).mockResolvedValueOnce({
      providers: [],
      models: [{ id: "minimax-m3", label: "MiniMax-M3" }],
    } as never);
    vi.mocked(agentAuthStatus).mockResolvedValueOnce({
      ready: true,
      providers: ["minimax-m3"],
      runtimeReady: true,
      synchronized: true,
      runtimeModels: ["minimax-m3"],
      defaultModelId: "minimax-m3",
    });
    render(<AutomationPanel />);
    fireEvent.click(await screen.findByText("+ 添加自动化"));

    fireEvent.click(await screen.findByRole("button", { name: /Auto.*MiniMax-M3/ }));
    fireEvent.click(screen.getByRole("button", { name: "MiniMax-M3 · 个人" }));
    expect(screen.getByRole("button", { name: /MiniMax-M3/ })).toBeInTheDocument();

    fireEvent.change(document.querySelector(".atm-modal-input") as HTMLInputElement, { target: { value: "每日报告" } });
    fireEvent.change(document.querySelector(".atm-prompt-textarea") as HTMLTextAreaElement, { target: { value: "生成报告" } });
    fireEvent.click(screen.getByRole("button", { name: "保存" }));

    await waitFor(() => expect(automationsSave).toHaveBeenCalledTimes(1));
    expect(vi.mocked(automationsSave).mock.calls[0][0]).toMatchObject({
      name: "每日报告",
      prompt: "生成报告",
      modelId: "minimax-m3",
    });
  });

  it("macOS 点击选项先失焦时仍可选择模型、保存并重新打开", async () => {
    beginRunnableEditor();
    const models = [
      { id: "ready-model", label: "原模型" },
      { id: "other-model", label: "另一个模型" },
    ];
    vi.mocked(providersList).mockResolvedValue({ providers: [], models } as never);
    render(<AutomationPanel />);
    const trigger = await screen.findByRole("button", { name: "原模型 · 个人" });
    fireEvent.click(trigger);
    const option = screen.getByRole("button", { name: "另一个模型 · 个人" });
    fireEvent.mouseDown(option, { button: 0 });
    // WebKit does not focus buttons clicked with the mouse. The focused Auto
    // or model option loses focus to the document before mouseup/click.
    act(() => (document.activeElement as HTMLElement).blur());
    fireEvent.mouseUp(option, { button: 0 });
    fireEvent.click(option);
    expect(useAutomationEditorStore.getState().draft?.modelId).toBe("other-model");
    expect(document.querySelector(".atm-chip-dropdown__menu")).toBeNull();
    expect(trigger).toHaveFocus();
    fireEvent.click(screen.getByRole("button", { name: "保存" }));
    await waitFor(() => expect(automationsSave).toHaveBeenCalledWith(expect.objectContaining({ modelId: "other-model" })));
    const saved = vi.mocked(automationsSave).mock.calls[0][0];
    act(() => useAutomationEditorStore.getState().begin(saved, saved));
    const selectedTrigger = await screen.findByRole("button", { name: "另一个模型 · 个人" });
    fireEvent.click(selectedTrigger);
    expect(screen.getAllByRole("button", { name: "另一个模型 · 个人" })[1]).toHaveClass("active");
    const auto = screen.getByRole("button", { name: /^Auto\s*每次运行/ });
    fireEvent.mouseDown(auto, { button: 0 });
    act(() => (document.activeElement as HTMLElement).blur());
    fireEvent.mouseUp(auto, { button: 0 });
    fireEvent.click(auto);
    expect(useAutomationEditorStore.getState().draft?.modelId).toBeUndefined();
    expect(screen.getByRole("button", { name: /Auto.*原模型/ })).toHaveAttribute("aria-expanded", "false");
    fireEvent.click(screen.getByRole("button", { name: "保存" }));
    await waitFor(() => expect(automationsSave).toHaveBeenCalledTimes(2));
    expect(vi.mocked(automationsSave).mock.calls[1][0].modelId).toBeUndefined();
  });

  it("模型菜单仍支持内部 Tab 导航、Tab 离开、Esc 和点击外部关闭", async () => {
    beginRunnableEditor();
    render(<AutomationPanel />);
    const trigger = await screen.findByRole("button", { name: "可用模型 · 个人" });
    const user = userEvent.setup();
    await user.click(trigger);
    expect(screen.getByRole("button", { name: /^Auto\s*每次运行/ })).toHaveFocus();
    await user.tab();
    expect(trigger).toHaveAttribute("aria-expanded", "true");
    await user.tab();
    expect(screen.getByRole("button", { name: "技能" })).toHaveFocus();
    expect(trigger).toHaveAttribute("aria-expanded", "false");
    await user.click(trigger);
    await user.keyboard("{Escape}");
    expect(trigger).toHaveFocus();
    expect(trigger).toHaveAttribute("aria-expanded", "false");
    await user.click(trigger);
    fireEvent.mouseDown(screen.getByText("任务要求"));
    expect(trigger).toHaveAttribute("aria-expanded", "false");
    expect(useAutomationEditorStore.getState().draft?.modelId).toBe("ready-model");
  });

  it.each(["技能", "召唤专家"])("%s 菜单也允许 WebKit 失焦后的内部点击", async (name) => {
    beginRunnableEditor();
    vi.mocked(skillsList).mockResolvedValue([
      { name: "first", displayName: "第一个技能", enabled: true },
      { name: "second", displayName: "第二个技能", enabled: true },
    ]);
    vi.mocked(agentsList).mockResolvedValue([{ name: "新闻专家", path: "/review/agents/news.md", scope: "project", raw: "News" }]);
    render(<AutomationPanel />);
    await waitFor(() => expect(screen.queryByText("正在加载工作空间、模型与运行时能力…")).toBeNull());
    fireEvent.click(screen.getByRole("button", { name }));
    const option = screen.getByRole("button", { name: name === "技能" ? "第二个技能" : "新闻专家" });
    const copy = option.querySelector("span:last-child")!;
    fireEvent.mouseDown(copy, { button: 0 });
    act(() => (document.activeElement as HTMLElement).blur());
    fireEvent.mouseUp(copy, { button: 0 });
    fireEvent.click(copy);
    const draft = useAutomationEditorStore.getState().draft;
    if (name === "技能") expect(draft?.skills).toEqual(["second"]);
    else expect(draft).toMatchObject({ expertId: "新闻专家", expertPath: "/review/agents/news.md" });
    expect(option).toHaveClass("active");
    expect(document.querySelector(".atm-chip-dropdown__menu")).toBeInTheDocument();
  });

  it("Auto 显示实际默认模型并保持自动选择语义", async () => {
    vi.mocked(providersList).mockResolvedValueOnce({
      providers: [],
      models: [{ id: "minimax-m3", label: "MiniMax-M3" }],
    } as never);
    vi.mocked(agentAuthStatus).mockResolvedValueOnce({
      ready: true,
      providers: ["minimax-m3"],
      runtimeReady: true,
      synchronized: true,
      runtimeModels: ["minimax-m3"],
      defaultModelId: "minimax-m3",
    });
    render(<AutomationPanel />);
    fireEvent.click(await screen.findByText("+ 添加自动化"));

    expect(await screen.findByRole("button", { name: /Auto.*MiniMax-M3/ })).toBeInTheDocument();
    fireEvent.change(document.querySelector(".atm-modal-input") as HTMLInputElement, { target: { value: "Auto 报告" } });
    fireEvent.change(document.querySelector(".atm-prompt-textarea") as HTMLTextAreaElement, { target: { value: "执行任务" } });
    fireEvent.click(screen.getByRole("button", { name: "保存" }));

    await waitFor(() => expect(automationsSave).toHaveBeenCalledTimes(1));
    expect(vi.mocked(automationsSave).mock.calls[0][0].modelId).toBeUndefined();
  });

  it("Auto 没有可用默认模型时阻止保存并给出可操作提示", async () => {
    const onToast = vi.fn();
    render(<AutomationPanel onToast={onToast} />);
    fireEvent.click(await screen.findByText("+ 添加自动化"));

    fireEvent.change(document.querySelector(".atm-modal-input") as HTMLInputElement, { target: { value: "Auto 报告" } });
    fireEvent.change(document.querySelector(".atm-prompt-textarea") as HTMLTextAreaElement, { target: { value: "执行任务" } });
    fireEvent.click(screen.getByRole("button", { name: "保存" }));

    expect(automationsSave).not.toHaveBeenCalled();
    expect(onToast).toHaveBeenCalledWith("当前没有可用的默认模型，请在「设置 → 模型」检查连接。");
  });

  it("点击模板卡片预填进入创建表单", async () => {
    render(<AutomationPanel />);
    await waitFor(() => screen.getByText("每日 AI 新闻推送"));
    fireEvent.click(screen.getByText("每日 AI 新闻推送"));
    await waitFor(() => {
      const nameInput = document.querySelector(".atm-modal-input") as HTMLInputElement;
      expect(nameInput?.value).toBe("每日 AI 新闻推送");
    });
  });

  it("频率切换：按间隔显示 每 N 小时，单次显示日期选择", async () => {
    render(<AutomationPanel />);
    await waitFor(() => screen.getByText("+ 添加自动化"));
    fireEvent.click(screen.getByText("+ 添加自动化"));
    await waitFor(() => screen.getByText("执行频率"));
    fireEvent.click(screen.getByText("按间隔"));
    expect(screen.getByText("每")).toBeInTheDocument();
    expect(screen.getByText("小时")).toBeInTheDocument();
    fireEvent.click(screen.getByText("单次"));
    // 单次模式下隐藏生效日期区间
    expect(screen.queryByText(/生效日期区间/)).not.toBeInTheDocument();
    // 切回周期恢复
    fireEvent.click(screen.getByText("周期"));
    expect(screen.getByText(/生效日期区间/)).toBeInTheDocument();
  });
});

describe("AutomationPermissionConfirmDialog 可访问性", () => {
  it("默认聚焦安全操作，支持 Tab 圈定、Escape 和焦点恢复", async () => {
    const onCancel = vi.fn();
    const onConfirm = vi.fn();
    const onFallback = vi.fn();
    const view = (open: boolean) => (
      <>
        <button type="button">打开权限确认</button>
        <AutomationPermissionConfirmDialog
          open={open}
          onConfirm={onConfirm}
          onCancel={onCancel}
          onFallbackToDefault={onFallback}
        />
      </>
    );
    const { rerender } = render(view(false));
    const invoker = screen.getByRole("button", { name: "打开权限确认" });
    invoker.focus();

    rerender(view(true));
    const dialog = screen.getByRole("alertdialog", { name: /完全访问权限运行/ });
    const cancel = within(dialog).getByRole("button", { name: "取消" });
    expect(cancel).toHaveFocus();

    const acknowledgement = within(dialog).getByRole("checkbox", { name: /我已了解风险/ });
    fireEvent.click(acknowledgement);
    const confirm = within(dialog).getByRole("button", { name: "确认授权" });
    await waitFor(() => expect(confirm).toBeEnabled());
    confirm.focus();
    fireEvent.keyDown(document, { key: "Tab" });
    expect(acknowledgement).toHaveFocus();

    fireEvent.keyDown(document, { key: "Escape" });
    expect(onCancel).toHaveBeenCalledTimes(1);
    rerender(view(false));
    expect(invoker).toHaveFocus();
  });
});

describe("AutomationPanel（有任务/有记录）", () => {
  const baseAutomation = {
    id: "a1",
    name: "每日 AI 新闻推送",
    prompt: "整理 AI 新闻",
    cwds: "",
    status: "ACTIVE" as const,
    skills: [],
    connectorIds: [],
    permissionMode: "fullAccess" as const,
    scheduleType: "recurring" as const,
    schedule: {
      freq: "DAILY" as const,
      interval: 1,
      byday: ["MO", "TU", "WE", "TH", "FR", "SA", "SU"],
      bymonthday: [],
      bymonth: [],
      byhour: 9,
      byminute: 0,
      intervalHours: 1,
    },
    pushToWeChat: false,
    createdAt: new Date().toISOString(),
  };

  it("危险操作确认弹窗默认聚焦取消、圈定 Tab 并在 Escape 后恢复触发器", async () => {
    snapshot = { automations: [baseAutomation], records: [] };
    render(<AutomationPanel />);

    const trigger = await screen.findByRole("button", { name: `打开「${baseAutomation.name}」更多操作` });
    trigger.focus();
    fireEvent.click(trigger);
    fireEvent.click(screen.getByRole("button", { name: "删除" }));

    const dialog = screen.getByRole("dialog", { name: `删除 ${baseAutomation.name}？` });
    const cancel = within(dialog).getByRole("button", { name: "取消" });
    const remove = within(dialog).getByRole("button", { name: "删除自动化任务" });
    expect(cancel).toHaveFocus();

    remove.focus();
    fireEvent.keyDown(document, { key: "Tab" });
    expect(cancel).toHaveFocus();

    fireEvent.keyDown(document, { key: "Escape" });
    expect(screen.queryByRole("dialog", { name: `删除 ${baseAutomation.name}？` })).toBeNull();
    expect(trigger).toHaveFocus();
  });

  it("编辑、测试运行和管理是独立控件，编辑不触发实际运行", async () => {
    snapshot = { automations: [baseAutomation], records: [] };
    render(<AutomationPanel />);
    const edit = await screen.findByRole("button", { name: `编辑自动化「${baseAutomation.name}」` });
    const run = screen.getByRole("button", { name: `测试运行「${baseAutomation.name}」` });
    const manage = screen.getByRole("button", { name: `打开「${baseAutomation.name}」更多操作` });
    for (const control of [edit, run, manage]) {
      expect(control.parentElement?.closest('button, [role="button"]')).toBeNull();
    }
    fireEvent.click(edit);
    expect(await screen.findByRole("textbox", { name: "名称" })).toHaveValue(baseAutomation.name);
    expect(automationsRun).not.toHaveBeenCalled();
  });

  it("批量选择通过独立复选控件完成，不进入编辑或执行任务", async () => {
    snapshot = { automations: [baseAutomation], records: [] };
    render(<AutomationPanel />);
    fireEvent.click(await screen.findByRole("button", { name: "批量管理" }));
    const select = screen.getByRole("checkbox", { name: `选择自动化「${baseAutomation.name}」` });
    expect(select).not.toBeChecked();
    fireEvent.click(select);
    expect(select).toBeChecked();
    expect(screen.queryByRole("textbox", { name: "名称" })).toBeNull();
    expect(automationsRun).not.toHaveBeenCalled();
  });

  it("任务列表：显示名称与调度摘要", async () => {
    snapshot = { automations: [baseAutomation], records: [] };
    render(<AutomationPanel />);
    await waitFor(() => expect(screen.getByText("每日 AI 新闻推送")).toBeInTheDocument());
    expect(screen.getByText("当前")).toBeInTheDocument();
    expect(screen.getByText("每天 09:00")).toBeInTheDocument();
    // 有任务时工具栏出现搜索/批量管理/添加自动化
    expect(screen.getByPlaceholderText("搜索自动化/记录")).toBeInTheDocument();
    expect(screen.getByText("批量管理")).toBeInTheDocument();
    expect(screen.getByText("添加自动化")).toBeInTheDocument();
  });

  it("运行记录：按天分组显示", async () => {
    snapshot = {
      automations: [baseAutomation],
      records: [
        {
          id: "r1",
          automationId: "a1",
          automationName: "每日 AI 新闻推送",
          status: "success",
          startedAt: new Date().toISOString(),
          finishedAt: new Date().toISOString(),
          archived: false,
        },
      ],
    };
    render(<AutomationPanel />);
    fireEvent.click(screen.getByText("运行记录"));
    await waitFor(() => expect(screen.getByText("今天")).toBeInTheDocument());
    expect(screen.getByText("每日 AI 新闻推送")).toBeInTheDocument();
    expect(screen.getByText("成功")).toBeInTheDocument();
  });

  it("运行记录：点击可打开对应 Agent 会话", async () => {
    const onOpenSession = vi.fn();
    snapshot = {
      automations: [baseAutomation],
      records: [
        {
          id: "r-session",
          automationId: "a1",
          automationName: "可查看结果的任务",
          status: "success",
          startedAt: new Date().toISOString(),
          finishedAt: new Date().toISOString(),
          sessionId: "session-123",
          cwd: "/workspace/project",
          archived: false,
        },
      ],
    };
    render(<AutomationPanel onOpenSession={onOpenSession} />);
    fireEvent.click(screen.getByText("运行记录"));
    const row = await screen.findByRole("button", { name: /可查看结果的任务/ });
    fireEvent.click(row);
    expect(onOpenSession).toHaveBeenCalledWith("session-123", "/workspace/project");
  });

  it("人工接管的运行可筛选、归档并打开会话，不显示失败重试", async () => {
    snapshot = {automations: [baseAutomation], records: [{
      id: "r-interrupted", automationId: "a1", automationName: "人工继续的任务", status: "interrupted",
      startedAt: new Date().toISOString(), finishedAt: new Date().toISOString(),
      sessionId: "manual-session", archived: false,
    }]};
    const onOpenSession = vi.fn();
    render(<AutomationPanel onOpenSession={onOpenSession} />);
    fireEvent.click(screen.getByText("运行记录"));
    expect(await screen.findByText("已人工接管")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", {name: "筛选运行记录"}));
    fireEvent.click(screen.getByRole("button", {name: "人工接管"}));
    fireEvent.click(screen.getByRole("button", {name: /人工继续的任务/}));
    expect(onOpenSession).toHaveBeenCalledWith("manual-session", undefined);
    expect(screen.queryByRole("button", {name: /重新运行/})).not.toBeInTheDocument();
    expect(screen.getByTitle("归档")).toBeEnabled();
  });

  it("失败记录可使用当前自动化配置重新运行", async () => {
    snapshot = {
      automations: [baseAutomation],
      records: [{
        id: "r-failed",
        automationId: "a1",
        automationName: "失败的任务",
        status: "failed",
        startedAt: new Date().toISOString(),
        finishedAt: new Date().toISOString(),
        error: "旧模型不可用",
        archived: false,
      }],
    };
    render(<AutomationPanel />);
    fireEvent.click(screen.getByText("运行记录"));

    fireEvent.click(await screen.findByRole("button", { name: "重新运行“失败的任务”" }));
    await waitFor(() => expect(automationsRun).toHaveBeenCalledWith("a1"));
  });

  it("排队中的任务展示真实状态并防止重复运行或删除记录", async () => {
    snapshot = {
      automations: [baseAutomation],
      records: [
        {
          id: "r-queued",
          automationId: "a1",
          automationName: "正在排队的任务",
          status: "queued",
          startedAt: new Date().toISOString(),
          archived: false,
        },
      ],
    };
    render(<AutomationPanel />);

    const runButton = await screen.findByLabelText("「每日 AI 新闻推送」正在运行");
    expect(runButton).toBeDisabled();

    fireEvent.click(screen.getByText("运行记录"));
    expect(await screen.findByText("等待调度")).toBeInTheDocument();
    expect(screen.queryByTitle("归档")).not.toBeInTheDocument();
    expect(screen.queryByTitle("删除")).not.toBeInTheDocument();
  });

  it("忽略晚到的旧快照响应", async () => {
    let resolveOld!: (value: AutomationSnapshot) => void;
    const oldRequest = new Promise<AutomationSnapshot>((resolve) => { resolveOld = resolve; });
    const freshSnapshot: AutomationSnapshot = {
      automations: [{ ...baseAutomation, id: "fresh", name: "新快照任务" }],
      records: [],
    };
    vi.mocked(automationsSnapshot)
      .mockImplementationOnce(() => oldRequest)
      .mockResolvedValueOnce(freshSnapshot);

    const { rerender } = render(<AutomationPanel onToast={() => {}} />);
    rerender(<AutomationPanel onToast={() => undefined} />);
    expect(await screen.findByText("新快照任务")).toBeInTheDocument();

    await act(async () => {
      resolveOld({
        automations: [{ ...baseAutomation, id: "old", name: "旧快照任务" }],
        records: [],
      });
      await oldRequest;
    });
    expect(screen.queryByText("旧快照任务")).not.toBeInTheDocument();
    expect(screen.getByText("新快照任务")).toBeInTheDocument();
  });

  it("后台调度事件会立即刷新运行记录", async () => {
    snapshot = { automations: [baseAutomation], records: [] };
    const { rerender } = render(<AutomationPanel refreshSignal={0} />);
    await waitFor(() => expect(automationsSnapshot).toHaveBeenCalledTimes(1));

    snapshot = {
      automations: [baseAutomation],
      records: [{
        id: "scheduled-run",
        automationId: baseAutomation.id,
        automationName: baseAutomation.name,
        status: "running",
        startedAt: new Date().toISOString(),
        archived: false,
      }],
    };
    rerender(<AutomationPanel refreshSignal={1} />);
    fireEvent.click(screen.getByText("运行记录"));

    expect(await screen.findByText("运行中")).toBeInTheDocument();
    expect(automationsSnapshot).toHaveBeenCalledTimes(2);
  });
});


describe("自动化专家稳定引用", () => {
  const ready = () => waitFor(() => expect(screen.queryByText("正在加载工作空间、模型与运行时能力…")).toBeNull());
  it("新选择保存稳定定义路径，并在编辑草稿中保留", async () => {
    beginRunnableEditor();
    vi.mocked(agentsList).mockResolvedValue([{name: "Identity Expert", scope: "project", path: "/tmp/.echo-agent/agents/stable.md", raw: "---\nname: Identity Expert\n---\nRole"}]);
    render(<AutomationPanel />); await ready();
    fireEvent.click(screen.getByRole("button", {name: "召唤专家"}));
    fireEvent.click(screen.getByRole("button", {name: "Identity Expert"}));
    expect(useAutomationEditorStore.getState().draft?.expertPath).toBe("/tmp/.echo-agent/agents/stable.md");
    fireEvent.click(screen.getByRole("button", {name: "保存"}));
    await waitFor(() => expect(automationsSave).toHaveBeenCalledWith(expect.objectContaining({expertId: "Identity Expert", expertName: "Identity Expert", expertPath: "/tmp/.echo-agent/agents/stable.md"})));
  });
  it("旧专家引用缺少路径时清楚提示重新选择，不因目录里存在同名专家而默默保存", async () => {
    beginRunnableEditor();
    useAutomationEditorStore.getState().setDraft(draft => draft && ({...draft, expertId: "Old Expert", expertName: "Old Expert"}));
    vi.mocked(agentsList).mockResolvedValue([{name: "Old Expert", scope: "project", path: "/tmp/.echo-agent/agents/replacement.md", raw: "Other role"}]);
    const toast = vi.fn(); render(<AutomationPanel onToast={toast} />); await ready();
    expect(screen.getByRole("button", {name: "Old Expert · 需重新选择"})).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", {name: "保存"}));
    expect(toast).toHaveBeenCalledWith(expect.stringContaining("重新选择专家")); expect(automationsSave).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", {name: "Old Expert · 需重新选择"}));
    fireEvent.click(screen.getByRole("button", {name: "Old Expert"}));
    fireEvent.click(screen.getByRole("button", {name: "保存"}));
    await waitFor(() => expect(automationsSave).toHaveBeenCalledWith(expect.objectContaining({expertPath: "/tmp/.echo-agent/agents/replacement.md"})));
  });
});
