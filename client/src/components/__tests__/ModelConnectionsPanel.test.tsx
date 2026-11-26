// @vitest-environment jsdom
import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import "@testing-library/jest-dom/vitest";
import { beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  providersList: vi.fn(),
  agentsDefaultsGet: vi.fn(),
  agentAuthStatus: vi.fn(),
  agentsDefaultsSave: vi.fn(),
  internalReload: vi.fn(),
  providersDeleteModel: vi.fn(),
  providersDeleteProvider: vi.fn(),
  providersFetchModelsForProvider: vi.fn(),
  providersSaveConnection: vi.fn(),
  providersSaveModel: vi.fn(),
  providersTestConnection: vi.fn(),
  providersTestModelConnection: vi.fn(),
  openExternalUrl: vi.fn(),
  orgSession: vi.fn(),
  orgSyncModelConfig: vi.fn(),
  listenOrgModelsChanged: vi.fn(),
}));

vi.mock("@/lib/agent-client", () => ({
  ...mocks,
}));

vi.mock("@/lib/org-client", () => ({
  orgSession: mocks.orgSession,
  orgSyncModelConfig: mocks.orgSyncModelConfig,
  listenOrgModelsChanged: mocks.listenOrgModelsChanged,
}));

import { ModelConnectionsPanel } from "../ModelConnectionsPanel";
import { resetOrgSessionMirror, useOrgSessionStore } from "@/stores/org-session-store";

async function waitForModelList() {
  await screen.findByRole("region", { name: "模型目录" });
}

function openMenu(scope: ReturnType<typeof within> | typeof screen, trigger: string) {
  fireEvent.click(scope.getByRole("button", { name: trigger }));
  return within(screen.getByRole("menu"));
}

function chooseService(dialog: HTMLElement, label: string) {
  const select = within(dialog).getByLabelText("供应商") as HTMLSelectElement;
  const option = Array.from(select.options).find(option => option.label === label);
  if (!option) throw new Error(`找不到供应商 ${label}`);
  fireEvent.change(select, { target: { value: option.value } });
}

function addModelButton() {
  return screen.findByRole("button", { name: "添加模型" });
}

function openAdvanced(dialog: HTMLElement) {
  const details = dialog.querySelector("details.model-connection-editor__advanced") as HTMLDetailsElement;
  if (!details.open) fireEvent.click(within(dialog).getByText("高级配置"));
}

const defaults = {
  defaultModel: "",
  defaultPermission: "",
  rememberToolApprovals: null,
};

const importProvider = {
  id: "personal",
  providerKind: "custom" as const,
  label: "工作连接",
  source: "personal" as const,
  baseUrl: "https://example.test/v1",
  credentialConfigured: true,
};

describe("ModelConnectionsPanel", () => {
  beforeEach(() => {
    vi.resetAllMocks();
    resetOrgSessionMirror();
    mocks.providersList.mockResolvedValue({ providers: [], models: [] });
    mocks.agentsDefaultsGet.mockResolvedValue(defaults);
    mocks.agentAuthStatus.mockResolvedValue({ ready: true, defaultModelId: undefined });
    mocks.internalReload.mockResolvedValue(undefined);
    mocks.providersTestModelConnection.mockReset();
    mocks.orgSession.mockResolvedValue({ loggedIn: false });
    mocks.listenOrgModelsChanged.mockResolvedValue(() => {});
  });

  it.each(["personal", "organization"] as const)("%s 模型可逐一测试，失败重试不影响默认配置", async source => {
    const provider = { ...importProvider, source, managed: source !== "personal" };
    if (source === "organization") mocks.orgSession.mockResolvedValue({ loggedIn: true, serverUrl: "https://org.test", bootstrap: { scopes: [{ id: "org", kind: "org", name: "组织" }] } });
    mocks.providersList.mockResolvedValue({ providers: [provider], models: [
      { modelId: "local/a", remoteModelId: "remote-a", name: "模型 A", providerId: provider.id, managed: provider.managed },
      { modelId: "local/b", remoteModelId: "remote-b", name: "模型 B", providerId: provider.id, managed: provider.managed },
    ] });
    let rejectTest!: (error: Error) => void;
    mocks.providersTestModelConnection.mockImplementationOnce(() => new Promise((_, reject) => { rejectTest = reject; }))
      .mockResolvedValueOnce(undefined);
    render(<ModelConnectionsPanel />);
    await waitForModelList();
    expect(screen.getByRole("button", { name: "测试模型 模型 B" })).toHaveAttribute("data-tip", "测试文本回复，可能消耗少量额度");
    const button = await screen.findByRole("button", { name: "测试模型 模型 B" });
    fireEvent.click(button);
    fireEvent.click(button);
    expect(button).toBeDisabled();
    expect(screen.getByRole("button", { name: "测试模型 模型 A" })).toBeEnabled();
    expect(mocks.providersTestModelConnection).toHaveBeenCalledExactlyOnceWith(expect.objectContaining({ id: provider.id, apiKey: undefined }), "remote-b");
    await act(async () => { rejectTest(new Error("模型接口返回 429：请稍后重试")); });
    const row = button.closest("li")!;
    const badge = within(row).getByRole("status");
    expect(badge).toHaveTextContent("失败");
    fireEvent.click(within(row).getByRole("button", { name: "查看 模型 B 的测试错误" }));
    expect(within(row).getByText("模型接口返回 429：请稍后重试")).toBeVisible();
    fireEvent.click(button);
    expect(await within(row).findByRole("status")).toHaveTextContent(/^(不足 )?[0-9.]+s$/);
    expect(within(row).queryByText(/429/)).not.toBeInTheDocument();
    expect(mocks.agentsDefaultsSave).not.toHaveBeenCalled();
    expect(mocks.providersSaveConnection).not.toHaveBeenCalled();
  });

  it("管理入口定位指定连接和模型，跨连接同名模型不会混淆", async () => {
    mocks.providersList.mockResolvedValue({ providers: [
      { ...importProvider, id: "org", source: "organization", label: "团队", managed: true },
      importProvider,
    ], models: [
      { modelId: "org/a", remoteModelId: "a", name: "同名模型", providerId: "org" },
      { modelId: "personal/a", remoteModelId: "a", name: "同名模型", providerId: "personal" },
    ] });
    const view = render(<ModelConnectionsPanel initialModelId="personal/a" />);
    const region = await screen.findByRole("region", { name: "工作连接" });
    expect(within(region).getByRole("listitem")).toHaveAttribute("data-model-id", "personal/a");
    expect(within(region).getByRole("listitem")).toHaveFocus();
    view.rerender(<ModelConnectionsPanel initialModelId="org/a" />);
    const team = await screen.findByRole("region", { name: "团队" });
    expect(within(team).getByRole("listitem")).toHaveAttribute("data-model-id", "org/a");
    expect(within(team).getByRole("listitem")).toHaveFocus();
  });

  it("中文输入 Model ID 时 Enter 不添加，关闭草稿先确认", async () => {
    render(<ModelConnectionsPanel />);
    await screen.findByRole("region", { name: "模型目录" });
    const opener = await addModelButton();
    opener.focus();
    fireEvent.click(opener);
    const dialog = screen.getByRole("dialog", { name: "添加模型" });
    chooseService(dialog, "自定义（OpenAI 兼容）");
    const input = within(dialog).getByLabelText("模型名称");
    fireEvent.change(input, { target: { value: "候选模型" } });
    fireEvent.keyDown(input, { key: "Enter", isComposing: true });
    expect(input).toHaveValue("候选模型");
    fireEvent.keyDown(input, { key: "Enter", keyCode: 229 });
    expect(input).toHaveValue("候选模型");
    fireEvent.keyDown(input, { key: "Enter" });
    expect(input).toHaveValue("候选模型");
    expect(within(dialog).queryByRole("button", { name: "添加更多模型" })).toBeNull();
    fireEvent.keyDown(document, { key: "Escape" });
    expect(screen.getByRole("alertdialog", { name: "舍弃未保存的修改？" })).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "继续编辑" }));
    expect(input).toHaveValue("候选模型");
    expect(mocks.providersSaveConnection).not.toHaveBeenCalled();
  });

  it("复用连接获取模型失败后可在添加模型里重试，等待时防止重复读取", async () => {
    mocks.providersList.mockResolvedValue({ providers: [importProvider], models: [] });
    let resolveModels!: (models: Array<{ id: string }>) => void;
    mocks.providersFetchModelsForProvider
      .mockRejectedValueOnce(new Error("连接中断"))
      .mockImplementationOnce(() => new Promise(resolve => { resolveModels = resolve; }));
    render(<ModelConnectionsPanel />);
    fireEvent.click(await addModelButton());
    const element = screen.getByRole("dialog", { name: "添加模型" });
    const dialog = within(element);
    chooseService(element, "工作连接");
    const discover = dialog.getByRole("button", { name: "从服务获取模型" });
    fireEvent.click(discover);
    expect(await dialog.findByRole("status")).toHaveTextContent("连接中断");
    fireEvent.click(discover);
    fireEvent.click(discover);
    expect(discover).toBeDisabled();
    expect(mocks.providersFetchModelsForProvider).toHaveBeenCalledTimes(2);
    expect(mocks.providersSaveConnection).not.toHaveBeenCalled();
    await act(async () => resolveModels([{ id: "model-a" }]));
    fireEvent.click(await dialog.findByRole("radio", { name: "model-a" }));
    expect(dialog.getByLabelText("模型名称")).toHaveValue("model-a");
  });

  it("明确展示组织托管来源且不提供编辑或删除入口", async () => {
    mocks.providersList.mockResolvedValue({
      providers: [{
        id: "echoagent-organization",
        providerKind: "custom",
        label: "组织提供",
        source: "organization",
        managed: true,
        credentialConfigured: true,
        baseUrl: "http://organization.example/v1",
        apiBackend: "chat_completions",
        authScheme: "bearer",
        syncedAt: 1_800_000_000_000,
      }],
      models: [{
        modelId: "organization/MiniMax-M3",
        remoteModelId: "MiniMax-M3",
        providerId: "echoagent-organization",
        name: "MiniMax-M3",
        managed: true,
      }],
    });

    render(<ModelConnectionsPanel />);
    await waitForModelList();

    const connection = within(await screen.findByRole("region", { name: "组织提供" }));
    expect(connection.getByText("MiniMax-M3")).toBeInTheDocument();
    const menu = openMenu(connection, "模型操作 MiniMax-M3");
    expect(menu.queryByRole("menuitem", { name: "编辑" })).toBeNull();
    expect(menu.queryByRole("menuitem", { name: "删除" })).toBeNull();
    expect(connection.getByText(/组织托管/)).toBeInTheDocument();
    expect(connection.queryByText(/organization\.example|HTTP 会明文传输/)).not.toBeInTheDocument();
  });

  it("兼容旧后端数据时排除已移除的内置连接和五个模型", async () => {
    mocks.providersList.mockResolvedValue({
      providers: [{ id: "echoagent-ojlab", label: "内置模型", providerKind: "custom", source: "builtin", managed: true }],
      models: ["Echo Brook", "Echo Cedar", "Echo Grove", "Echo Reed", "Echo Willow"].map(name => ({
        modelId: `echoagent-ojlab/${name}`, providerId: "echoagent-ojlab", name,
      })),
    });
    render(<ModelConnectionsPanel />);
    expect(await screen.findByRole("region", { name: "模型目录" })).toBeInTheDocument();
    expect(screen.queryByText("内置模型")).not.toBeInTheDocument();
    expect(screen.queryByText(/Echo Brook|Echo Cedar|Echo Grove|Echo Reed|Echo Willow/)).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "添加模型" })).toBeEnabled();
  });

  it("目录只展示模型和来源，能力字段放在编辑表单中", async () => {
    mocks.providersList.mockResolvedValue({
      providers: [importProvider],
      models: [{ modelId: "model-a", providerId: importProvider.id }, { modelId: "model-b", providerId: importProvider.id }],
      modelCapabilities: { "model-a": { tools: true, images: true }, "model-b": { images: false } },
    });
    render(<ModelConnectionsPanel />);
    const models = within(await screen.findByRole("region", { name: "工作连接" })).getAllByRole("listitem");
    expect(within(models[0]).getByText("工作连接")).toBeVisible();
    expect(within(models[0]).queryByText(/工具调用|图片输入/)).toBeNull();
    expect(within(models[1]).queryByText(/工具调用|图片输入/)).not.toBeInTheDocument();
    expect(screen.queryByRole("combobox", { name: "默认模型" })).toBeNull();
    expect(screen.getByRole("button", { name: "添加模型" })).toBeVisible();
    expect(screen.queryByRole("heading", { name: "输入建议" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "管理模型连接" })).not.toBeInTheDocument();
  });

  it("未命名的自定义连接显示协议类型，接口地址仍可在编辑时查看", async () => {
    const provider = { ...importProvider, label: undefined, baseUrl: "https://gateway.example.test:8443/v1" };
    mocks.providersList.mockResolvedValue({ providers: [provider], models: [{ modelId: "model-a", providerId: provider.id }] });
    mocks.providersSaveConnection.mockResolvedValue({ providerId: provider.id, modelIds: ["model-a"] });
    render(<ModelConnectionsPanel />);
    const modelGroup = await screen.findByRole("region", { name: "OpenAI 兼容" });
    expect(within(modelGroup).getByText("OpenAI 兼容")).toBeVisible();
    expect(modelGroup).not.toHaveTextContent("gateway.example.test");
    fireEvent.click(screen.getByRole("button", { name: "编辑模型 model-a" }));
    const dialog = within(screen.getByRole("dialog", { name: "编辑模型" }));
    expect(dialog.getByLabelText("接口地址")).toHaveValue(provider.baseUrl);
    fireEvent.click(dialog.getByRole("button", { name: "保存模型" }));
    await waitFor(() => expect(mocks.providersSaveConnection).toHaveBeenCalledWith(
      expect.objectContaining({ id: provider.id, label: provider.label, baseUrl: provider.baseUrl }),
      [expect.objectContaining({ modelId: "model-a" })], false,
    ));
  });

  it("模型目录读取成功但运行时未就绪时提供重试", async () => {
    mocks.agentAuthStatus.mockResolvedValue({ ready: false, reason: "Runtime 未启动" });
    mocks.providersList.mockResolvedValue({ providers: [importProvider], models: [{ modelId: "m", providerId: importProvider.id }] });
    render(<ModelConnectionsPanel />);
    const notice = await screen.findByRole("alert");
    expect(notice).toHaveTextContent("模型暂未就绪。Runtime 未启动");
    expect(within(notice).getByRole("button", { name: "重试加载" })).toBeEnabled();
  });

  it("在同一个模型编辑表单保存连接字段和模型配置，保留同服务的其他模型", async () => {
    mocks.providersList.mockResolvedValue({
      providers: [{ id: "custom", providerKind: "custom", source: "personal", label: "OpenAI 兼容", baseUrl: "https://example.com/v1", contextWindow: 16000 }],
      models: [
        { modelId: "model-a", remoteModelId: "model-a", providerId: "custom", maxOutputTokens: 8192 },
        { modelId: "model-b", remoteModelId: "model-b", providerId: "custom" },
      ],
    });
    mocks.providersSaveConnection.mockResolvedValue({ providerId: "custom", modelIds: ["model-a"] });
    render(<ModelConnectionsPanel />);
    await waitForModelList();
    fireEvent.click(openMenu(screen, "模型操作 model-a").getByRole("menuitem", { name: "编辑" }));
    const dialog = screen.getByRole("dialog", { name: "编辑模型" });
    const editor = within(dialog);
    const sharedNote = editor.getByRole("note");
    expect(sharedNote).toHaveTextContent("此连接由 2 个模型共用");
    expect(sharedNote).toHaveTextContent(/API Key.*会同时影响这些模型/);
    expect(editor.getByLabelText("接口地址")).toHaveAccessibleDescription(sharedNote.textContent?.trim());
    expect(editor.getByLabelText("API Key")).toHaveAccessibleDescription(sharedNote.textContent?.trim());
    expect(editor.getByLabelText("模型名称")).not.toHaveAccessibleDescription();
    const maxOutput = editor.getByLabelText("输出，最大输出（tokens）");
    expect(within(dialog).getByLabelText("供应商")).toHaveValue("custom");
    expect(within(dialog).getByLabelText("模型名称")).toHaveValue("model-a");
    expect(within(dialog).getByLabelText("API Key")).toHaveValue("");
    fireEvent.change(within(dialog).getByLabelText("接口地址"), { target: { value: "https://example.com/v2" } });
    fireEvent.click(within(dialog).getByRole("checkbox", { name: "图片输入" }));
    expect(maxOutput).toHaveValue(8192);
    fireEvent.change(maxOutput, { target: { value: "20000" } });
    fireEvent.click(within(dialog).getByRole("button", { name: "保存模型" }));
    expect(within(dialog).getByText("最大输出不能超过上下文窗口。")).toBeInTheDocument();
    expect(mocks.providersSaveConnection).not.toHaveBeenCalled();
    fireEvent.change(maxOutput, { target: { value: "" } });
    fireEvent.click(within(dialog).getByRole("button", { name: "保存模型" }));
    await waitFor(() => expect(mocks.providersSaveConnection).toHaveBeenCalledWith(
      expect.objectContaining({ id: "custom", label: "OpenAI 兼容", baseUrl: "https://example.com/v2", apiKey: undefined }),
      [expect.objectContaining({ modelId: "model-a", clearMaxOutputTokens: true, maxOutputTokens: undefined, capabilityOverrides: { images: true } })],
      false,
    ));
  });

  it("官方 MiniMax 连接使用已知能力，并在关闭调节后隐藏思考强度", async () => {
    const modelId = "MiniMax-M3.1-Flash";
    mocks.providersList.mockResolvedValue({
      providers: [{ ...importProvider, baseUrl: "https://api.minimax.cn/v1", apiBackend: "chat_completions" }],
      models: [{ modelId, providerId: importProvider.id }],
    });
    mocks.providersSaveConnection.mockResolvedValue({ providerId: importProvider.id, modelIds: [modelId] });
    render(<ModelConnectionsPanel />);
    fireEvent.click(await screen.findByRole("button", { name: `编辑模型 ${modelId}` }));
    const dialog = within(screen.getByRole("dialog", { name: "编辑模型" }));
    for (const name of ["工具调用", "图片输入", "调节思考强度"]) {
      expect(dialog.getByRole("checkbox", { name })).toBeChecked();
    }
    const effort = dialog.getByLabelText("默认思考强度");
    expect(effort).toHaveValue("");
    expect(within(effort).queryByRole("option", { name: "关闭" })).toBeNull();
    expect(within(effort).getByRole("option", { name: "极致" })).toHaveValue("max");

    const thinking = dialog.getByRole("checkbox", { name: "调节思考强度" });
    fireEvent.click(thinking);
    expect(dialog.queryByLabelText("默认思考强度")).toBeNull();
    expect(dialog.getByText(/该模型始终开启思考/)).toBeVisible();
    fireEvent.click(thinking);
    fireEvent.change(dialog.getByLabelText("默认思考强度"), { target: { value: "high" } });
    fireEvent.click(dialog.getByRole("button", { name: "保存模型" }));
    await waitFor(() => expect(mocks.providersSaveConnection).toHaveBeenCalledWith(
      expect.anything(),
      [expect.objectContaining({ capabilityOverrides: { reasoningEffort: "high" } })],
      false,
    ));
  });

  it("任意自定义模型可以声明已确认的能力和思考档位", async () => {
    mocks.providersList.mockResolvedValue({
      providers: [importProvider],
      models: [{ modelId: "unknown-model", providerId: importProvider.id }],
    });
    mocks.providersSaveConnection.mockResolvedValue({ providerId: importProvider.id, modelIds: ["unknown-model"] });
    render(<ModelConnectionsPanel />);
    fireEvent.click(await screen.findByRole("button", { name: "编辑模型 unknown-model" }));
    const dialog = within(screen.getByRole("dialog", { name: "编辑模型" }));
    expect(dialog.getByRole("checkbox", { name: "工具调用" })).toBePartiallyChecked();
    expect(dialog.queryByLabelText("默认思考强度")).toBeNull();
    fireEvent.click(dialog.getByRole("checkbox", { name: "工具调用" }));
    fireEvent.click(dialog.getByRole("checkbox", { name: "思考模式" }));
    fireEvent.click(dialog.getByRole("checkbox", { name: "高" }));
    fireEvent.change(dialog.getByLabelText("默认思考强度"), { target: { value: "high" } });
    fireEvent.click(dialog.getByRole("button", { name: "保存模型" }));
    await waitFor(() => expect(mocks.providersSaveConnection).toHaveBeenCalledWith(
      expect.anything(),
      [expect.objectContaining({
        capabilityOverrides: expect.objectContaining({
          tools: true, reasoning: true, reasoningEffort: "high", reasoningEfforts: ["high"],
        }),
      })],
      false,
    ));
  });

  it("从模型行设为应用默认，并可恢复自动选择", async () => {
    let saved = { ...defaults, defaultPermission: "allow_once", rememberToolApprovals: false };
    mocks.agentsDefaultsGet.mockImplementation(async () => saved);
    mocks.agentsDefaultsSave.mockImplementation(async next => { saved = next; });
    mocks.providersList.mockResolvedValue({
      providers: [importProvider], models: [{ modelId: "personal/a", name: "工作模型", providerId: importProvider.id }],
    });
    render(<ModelConnectionsPanel />);
    await waitForModelList();
    fireEvent.click(openMenu(screen, "模型操作 工作模型").getByRole("menuitem", { name: "设为默认" }));
    expect(await screen.findByText("应用默认模型 · 已设默认 · 工作模型")).toBeVisible();
    expect(mocks.agentsDefaultsSave).toHaveBeenLastCalledWith({
      defaultModel: "personal/a", defaultPermission: "allow_once", rememberToolApprovals: false,
    });
    fireEvent.click(screen.getByRole("button", { name: "恢复自动选择" }));
    expect(await screen.findByText("应用默认模型 · 自动选择")).toBeVisible();
    expect(mocks.agentsDefaultsSave).toHaveBeenLastCalledWith(expect.objectContaining({ defaultModel: "" }));
  });

  it("删除无模型的连接时只提交受空连接保护的删除请求", async () => {
    mocks.providersList.mockResolvedValueOnce({ providers: [importProvider], models: [] })
      .mockResolvedValue({ providers: [], models: [] });
    mocks.providersDeleteProvider.mockResolvedValue(undefined);
    render(<ModelConnectionsPanel />);
    fireEvent.click(await screen.findByText("无模型的连接（1）"));
    fireEvent.click(screen.getByRole("button", { name: "删除连接 工作连接" }));
    const dialog = screen.getByRole("alertdialog", { name: "删除连接「工作连接」？" });
    expect(dialog).toHaveTextContent("API Key");
    fireEvent.click(within(dialog).getByRole("button", { name: "删除连接" }));
    expect(await screen.findByText("连接已删除。")).toBeVisible();
    expect(mocks.providersDeleteProvider).toHaveBeenCalledExactlyOnceWith(importProvider.id, true);
  });

  it("切换到 MiniMax 时清除来自自定义供应商的无效档位", async () => {
    mocks.providersSaveConnection.mockResolvedValue({ providerId: "minimax", modelIds: ["MiniMax-M3.1-Flash"] });
    render(<ModelConnectionsPanel />);
    fireEvent.click(await addModelButton());
    const editor = screen.getByRole("dialog", { name: "添加模型" });
    const dialog = within(editor);
    fireEvent.change(dialog.getByLabelText("供应商"), { target: { value: "custom" } });
    fireEvent.change(dialog.getByLabelText("模型名称"), { target: { value: "MiniMax-M3.1-Flash" } });
    fireEvent.click(dialog.getByRole("checkbox", { name: "思考模式" }));
    fireEvent.click(dialog.getByRole("checkbox", { name: "关闭" }));
    fireEvent.change(dialog.getByLabelText("默认思考强度"), { target: { value: "none" } });
    fireEvent.change(dialog.getByLabelText("供应商"), { target: { value: "minimax" } });
    fireEvent.change(dialog.getByLabelText("API Key"), { target: { value: "isolated-test-key" } });
    openAdvanced(editor);
    expect(dialog.getByText("模型或连接已更换，请重新检查能力与思考档位。")).toBeVisible();
    expect(dialog.getByLabelText("默认思考强度")).toHaveValue("");
    expect(dialog.queryByRole("option", { name: "关闭" })).toBeNull();
    fireEvent.click(dialog.getByRole("button", { name: "保存模型" }));
    await waitFor(() => expect(mocks.providersSaveConnection).toHaveBeenCalledWith(
      expect.anything(), [expect.objectContaining({ capabilityOverrides: undefined })], true,
    ));
  });

  it("MiniMax 的 Messages 协议使用带版本的接口地址并保留已知能力", async () => {
    render(<ModelConnectionsPanel />);
    fireEvent.click(await addModelButton());
    const editor = screen.getByRole("dialog", { name: "添加模型" });
    const dialog = within(editor);
    fireEvent.change(dialog.getByLabelText("供应商"), { target: { value: "minimax" } });
    fireEvent.change(dialog.getByLabelText("模型名称"), { target: { value: "MiniMax-M3.1-Flash" } });
    openAdvanced(editor);
    fireEvent.click(dialog.getByRole("checkbox", { name: "自定义协议" }));
    fireEvent.change(dialog.getByLabelText("请求协议"), { target: { value: "messages" } });
    expect(dialog.getByLabelText("接口地址")).toHaveValue("https://api.minimax.cn/anthropic/v1");
    expect(dialog.getByLabelText("调节思考强度")).toBeChecked();
    fireEvent.click(dialog.getByRole("checkbox", { name: "自定义协议" }));
    expect(dialog.getByLabelText("接口地址")).toHaveValue("https://api.minimax.cn/v1");
  });

  it("复用连接时可发现尚未添加的模型并设置最大输出", async () => {
    mocks.providersList.mockResolvedValue({
      providers: [{ id: "custom", providerKind: "custom", source: "personal", baseUrl: "https://example.com/v1", contextWindow: 16000 }],
      models: [{ modelId: "custom/existing-model", providerId: "custom", remoteModelId: "existing-model" }],
    });
    mocks.providersFetchModelsForProvider.mockResolvedValue([{ id: "existing-model" }, { id: "model-a" }]);
    mocks.providersSaveConnection.mockResolvedValue({ providerId: "custom", modelIds: ["model-a"] });
    render(<ModelConnectionsPanel />);
    await waitForModelList();
    await screen.findByRole("region", { name: "OpenAI 兼容" });
    fireEvent.click(await addModelButton());
    const dialogEl = screen.getByRole("dialog", { name: "添加模型" });
    const dialog = within(dialogEl);
    chooseService(dialogEl, "example.com");
    expect(dialog.queryByLabelText("API Key")).not.toBeInTheDocument();
    fireEvent.click(dialog.getByRole("button", { name: "从服务获取模型" }));
    fireEvent.click(await dialog.findByRole("radio", { name: "model-a" }));
    expect(dialog.queryByRole("radio", { name: "existing-model" })).not.toBeInTheDocument();
    expect(mocks.providersFetchModelsForProvider).toHaveBeenCalledWith("custom");
    expect(mocks.providersTestConnection).not.toHaveBeenCalled();
    openAdvanced(dialogEl);
    fireEvent.change(dialog.getByLabelText("输出，最大输出（tokens）"), { target: { value: "4096" } });
    fireEvent.click(dialog.getByRole("button", { name: "保存模型" }));
    await waitFor(() => expect(mocks.providersSaveConnection).toHaveBeenCalledWith(
      expect.objectContaining({ id: "custom" }),
      [expect.objectContaining({ remoteModelId: "model-a", maxOutputTokens: 4096 })],
      false,
    ));
  });

  it("展示连接返回的全部模型而不截断列表", async () => {
    const provider = {
      id: "custom",
      providerKind: "custom" as const,
      label: "工作用",
      source: "personal" as const,
      credentialConfigured: true,
      baseUrl: "https://api.example.com/v1",
      apiBackend: "chat_completions" as const,
      authScheme: "bearer" as const,
    };
    const models = Array.from({ length: 7 }, (_, index) => ({
      modelId: `codex/model-${index + 1}`,
      remoteModelId: `model-${index + 1}`,
      providerId: provider.id,
      name: `Model ${index + 1}`,
    }));
    mocks.providersList.mockResolvedValue({ providers: [provider], models });

    render(<ModelConnectionsPanel />);
    await waitForModelList();

    const detail = await screen.findByRole("region", { name: "工作用" });
    expect(within(detail).getAllByRole("listitem")).toHaveLength(7);
    expect(within(detail).getByText("Model 7", { exact: true })).toHaveAttribute("title", "model-7");
  });

  it("新增表单只保存配置，不发送实际模型请求", async () => {
    mocks.providersTestModelConnection.mockResolvedValue(undefined);
    mocks.providersSaveConnection.mockResolvedValue({
      providerId: "custom",
      modelIds: ["MiniMax-M3"],
    });

    render(<ModelConnectionsPanel />);
    await screen.findByRole("region", { name: "模型目录" });
    fireEvent.click(await addModelButton());

    const dialog = screen.getByRole("dialog", { name: "添加模型" });
    chooseService(dialog, "自定义（OpenAI 兼容）");
    expect(within(dialog).getByLabelText("供应商")).toHaveFocus();
    expect(within(dialog).queryByLabelText("请求协议")).toBeNull();
    fireEvent.click(within(dialog).getByRole("checkbox", { name: "自定义协议" }));
    expect(within(dialog).getByLabelText("请求协议")).toBeVisible();
    fireEvent.change(within(dialog).getByLabelText("请求协议"), { target: { value: "responses" } });
    fireEvent.change(within(dialog).getByLabelText("接口地址"), { target: { value: "https://api.minimaxi.com/v1" } });
    fireEvent.change(within(dialog).getByLabelText("API Key"), { target: { value: "sk-test" } });
    fireEvent.change(within(dialog).getByLabelText("模型名称"), { target: { value: "MiniMax-M3" } });
    expect(within(dialog).queryByRole("button", { name: "测试此模型" })).toBeNull();
    expect(within(dialog).queryByRole("button", { name: "添加更多模型" })).toBeNull();
    fireEvent.click(within(dialog).getByRole("button", { name: "保存模型" }));
    await waitFor(() => {
      expect(mocks.providersTestModelConnection).not.toHaveBeenCalled();
      expect(mocks.providersSaveConnection).toHaveBeenCalledWith(
        expect.objectContaining({
          id: "",
          label: "",
          baseUrl: "https://api.minimaxi.com/v1",
          apiKey: "sk-test",
          providerKind: "custom",
          apiBackend: "responses",
        }),
        [expect.objectContaining({ modelId: "", remoteModelId: "MiniMax-M3" })],
        true,
      );
    });
    expect(mocks.internalReload).toHaveBeenCalledWith("models");
  });

  it("原内置服务地址作为个人连接时也必须明确配置凭据", async () => {
    render(<ModelConnectionsPanel />);
    await screen.findByRole("region", { name: "模型目录" });
    fireEvent.click(await addModelButton());
    const dialog = screen.getByRole("dialog", { name: "添加模型" });
    chooseService(dialog, "自定义（OpenAI 兼容）");
    fireEvent.change(within(dialog).getByLabelText("接口地址"), { target: { value: "http://123.56.188.16:8088/v1" } });
    fireEvent.change(within(dialog).getByLabelText("模型名称"), { target: { value: "my-model" } });
    expect(within(dialog).getByLabelText("API Key")).toBeEnabled();
    fireEvent.click(within(dialog).getByLabelText(/允许此个人连接使用 HTTP/));
    fireEvent.click(within(dialog).getByRole("button", { name: "保存模型" }));
    expect(await within(dialog).findByText("请填写 API 密钥。")).toBeInTheDocument();
    expect(mocks.providersSaveConnection).not.toHaveBeenCalled();
  });

  it("OpenAI 作为 Responses 模型 Provider 接入而不是独立 Agent Runtime", async () => {
    mocks.providersSaveConnection.mockResolvedValue({ providerId: "openai", modelIds: ["review-model"] });
    render(<ModelConnectionsPanel />);
    await screen.findByRole("region", { name: "模型目录" });
    fireEvent.click(await addModelButton());

    const dialog = screen.getByRole("dialog", { name: "添加模型" });
    chooseService(dialog, "自定义（OpenAI 兼容）");
    chooseService(dialog, "OpenAI");
    openAdvanced(dialog);
    expect(within(dialog).getByLabelText("接口地址")).toHaveValue("https://api.openai.com/v1");
    fireEvent.click(within(dialog).getByRole("checkbox", { name: "自定义协议" }));
    expect(within(dialog).getByLabelText("请求协议")).toHaveValue("responses");
    fireEvent.change(within(dialog).getByLabelText("API Key"), { target: { value: "fixture-key" } });
    fireEvent.change(within(dialog).getByLabelText("模型名称"), { target: { value: "review-model" } });
    fireEvent.click(within(dialog).getByRole("button", { name: "保存模型" }));
    await waitFor(() => expect(mocks.providersSaveConnection).toHaveBeenCalledWith(
      expect.objectContaining({ apiBackend: "responses", authScheme: "bearer" }),
      [expect.objectContaining({ remoteModelId: "review-model" })], true,
    ));
  });

  it("连接编辑器具有初始焦点、Tab 圈定、Escape 与触发器恢复", async () => {
    render(<ModelConnectionsPanel />);
    await screen.findByRole("region", { name: "模型目录" });
    const opener = await addModelButton();
    opener.focus();
    fireEvent.click(opener);

    const dialog = screen.getByRole("dialog", { name: "添加模型" });
    expect(within(dialog).getByLabelText("供应商")).toHaveFocus();
    const last = within(dialog).getByRole("button", { name: "取消" });
    last.focus();
    fireEvent.keyDown(document, { key: "Tab" });
    expect(within(dialog).getByRole("button", { name: "关闭" })).toHaveFocus();

    fireEvent.keyDown(document, { key: "Escape" });
    expect(screen.queryByRole("dialog", { name: "添加模型" })).toBeNull();
    expect(opener).toHaveFocus();
  });

  it("保存后等待 Runtime 和全局模型状态完全刷新", async () => {
    mocks.providersSaveConnection.mockResolvedValue({
      providerId: "custom",
      modelIds: ["model-a"],
    });
    let resolveReload!: () => void;
    mocks.internalReload.mockImplementation(() => new Promise<void>((resolve) => {
      resolveReload = resolve;
    }));
    let resolveRefresh!: () => void;
    const onModelsChanged = vi.fn(() => new Promise<void>((resolve) => {
      resolveRefresh = resolve;
    }));

    render(<ModelConnectionsPanel onModelsChanged={onModelsChanged} />);
    await screen.findByRole("region", { name: "模型目录" });
    fireEvent.click(await addModelButton());
    const dialog = screen.getByRole("dialog", { name: "添加模型" });
    chooseService(dialog, "自定义（OpenAI 兼容）");
    fireEvent.change(within(dialog).getByLabelText("接口地址"), { target: { value: "https://api.example.com/v1" } });
    fireEvent.change(within(dialog).getByLabelText("API Key"), { target: { value: "sk-test" } });
    fireEvent.change(within(dialog).getByLabelText("模型名称"), { target: { value: "model-a" } });
    fireEvent.click(within(dialog).getByRole("button", { name: "保存模型" }));

    await waitFor(() => expect(mocks.internalReload).toHaveBeenCalledWith("models"));
    expect(onModelsChanged).not.toHaveBeenCalled();

    await act(async () => resolveReload());
    await waitFor(() => expect(onModelsChanged).toHaveBeenCalledTimes(1));
    expect(screen.queryByText("模型已添加。")).not.toBeInTheDocument();

    await act(async () => resolveRefresh());
    expect(await screen.findByText("模型已添加。")).toBeInTheDocument();
  });

  it("编辑模型时留空密钥并由原生层复用已保存凭据", async () => {
    const provider = {
      id: "custom",
      providerKind: "custom" as const,
      label: "现有连接",
      source: "personal" as const,
      credentialConfigured: true,
      baseUrl: "https://api.example.com/v1",
      apiBackend: "chat_completions" as const,
      authScheme: "bearer" as const,
    };
    mocks.providersList.mockResolvedValue({
      providers: [provider],
      models: [{ modelId: "model-a", remoteModelId: "model-a", providerId: "custom" }],
    });
    mocks.providersTestModelConnection.mockResolvedValue(undefined);
    mocks.providersSaveConnection.mockResolvedValue({ providerId: "custom", modelIds: ["model-a"] });

    render(<ModelConnectionsPanel />);
    await waitForModelList();
    await screen.findByRole("region", { name: "现有连接" });
    fireEvent.click(screen.getByRole("button", { name: "编辑模型 model-a" }));
    const dialog = screen.getByRole("dialog", { name: "编辑模型" });
    expect(within(dialog).getByLabelText("API Key")).toHaveValue("");
    expect(within(dialog).queryByRole("note")).toBeNull();
    fireEvent.click(within(dialog).getByRole("button", { name: "保存模型" }));
    await waitFor(() => expect(mocks.providersSaveConnection).toHaveBeenCalledWith(
      expect.objectContaining({ id: "custom", apiKey: undefined, source: "personal" }),
      [expect.objectContaining({ modelId: "model-a", remoteModelId: "model-a" })], false,
    ));
    expect(mocks.providersTestModelConnection).not.toHaveBeenCalled();
  });

  it("服务端不支持 models 接口时仍可手填 Model ID 并保存", async () => {
    mocks.providersTestConnection.mockRejectedValue(new Error("404 /models"));
    mocks.providersSaveConnection.mockResolvedValue({
      providerId: "custom",
      modelIds: ["private-model-v2"],
    });

    render(<ModelConnectionsPanel />);
    await screen.findByRole("region", { name: "模型目录" });
    fireEvent.click(await addModelButton());
    const dialog = screen.getByRole("dialog", { name: "添加模型" });
    chooseService(dialog, "自定义（OpenAI 兼容）");
    fireEvent.change(within(dialog).getByLabelText("接口地址"), { target: { value: "https://private.example/v1" } });
    fireEvent.change(within(dialog).getByLabelText("API Key"), { target: { value: "private-key" } });
    fireEvent.change(within(dialog).getByLabelText("模型名称"), { target: { value: "private-model-v2" } });

    fireEvent.click(within(dialog).getByRole("button", { name: "测试连接" }));
    expect(await within(dialog).findByText(/可手动填写模型 ID 后保存/)).toBeInTheDocument();
    fireEvent.click(within(dialog).getByRole("button", { name: "保存模型" }));

    await waitFor(() => {
      expect(mocks.providersSaveConnection).toHaveBeenCalledWith(
        expect.objectContaining({
          label: "",
          baseUrl: "https://private.example/v1",
          apiKey: "private-key",
        }),
        [expect.objectContaining({ remoteModelId: "private-model-v2" })],
        true,
      );
    });
  });

  it("在请求前指出端口位置错误，并要求明确同意远程 HTTP", async () => {
    mocks.providersTestConnection.mockResolvedValue([{ id: "chat-xc" }]);
    render(<ModelConnectionsPanel />);
    await screen.findByRole("region", { name: "模型目录" });
    fireEvent.click(await addModelButton());
    const dialog = screen.getByRole("dialog", { name: "添加模型" });
    chooseService(dialog, "自定义（OpenAI 兼容）");
    const baseUrl = within(dialog).getByLabelText("接口地址");
    fireEvent.change(baseUrl, { target: { value: "http://example.com/:60100/v1" } });
    fireEvent.change(within(dialog).getByLabelText("API Key"), { target: { value: "test-key" } });
    fireEvent.click(within(dialog).getByRole("button", { name: "测试连接" }));
    expect(within(dialog).getByText(/端口号位置不对/)).toBeInTheDocument();
    expect(mocks.providersTestConnection).not.toHaveBeenCalled();

    fireEvent.change(baseUrl, { target: { value: "http://example.com:60100/v1" } });
    fireEvent.click(within(dialog).getByRole("button", { name: "测试连接" }));
    expect(within(dialog).getByText(/使用 HTTP 前，请确认/)).toBeInTheDocument();
    expect(mocks.providersTestConnection).not.toHaveBeenCalled();

    fireEvent.click(within(dialog).getByLabelText(/允许此个人连接使用 HTTP/));
    fireEvent.change(within(dialog).getByLabelText("API Key"), { target: { value: "test-key" } });
    fireEvent.click(within(dialog).getByRole("button", { name: "测试连接" }));
    await waitFor(() => expect(mocks.providersTestConnection).toHaveBeenCalledWith(
      expect.objectContaining({ baseUrl: "http://example.com:60100/v1", allowInsecureHttp: true }),
    ));

    fireEvent.change(baseUrl, { target: { value: "http://123.56.188.16:8088/v1" } });
    fireEvent.click(within(dialog).getByRole("button", { name: "测试连接" }));
    expect(within(dialog).getByText(/使用 HTTP 前，请确认/)).toBeInTheDocument();
    expect(mocks.providersTestConnection).toHaveBeenCalledTimes(1);
    fireEvent.click(within(dialog).getByLabelText(/允许此个人连接使用 HTTP/));
    fireEvent.change(within(dialog).getByLabelText("API Key"), { target: { value: "test-key" } });
    fireEvent.click(within(dialog).getByRole("button", { name: "测试连接" }));
    await waitFor(() => expect(mocks.providersTestConnection).toHaveBeenCalledTimes(2));
    expect(mocks.providersTestConnection).toHaveBeenLastCalledWith(
      expect.objectContaining({ baseUrl: "http://123.56.188.16:8088/v1", allowInsecureHttp: true }),
    );

    fireEvent.change(baseUrl, { target: { value: "https://secure.example.com/v1" } });
    fireEvent.change(within(dialog).getByLabelText("API Key"), { target: { value: "test-key" } });
    expect(within(dialog).queryByLabelText(/允许此个人连接使用 HTTP/)).not.toBeInTheDocument();
    fireEvent.click(within(dialog).getByRole("button", { name: "测试连接" }));
    await waitFor(() => expect(mocks.providersTestConnection).toHaveBeenCalledTimes(3));
    expect(mocks.providersTestConnection).toHaveBeenLastCalledWith(
      expect.objectContaining({ baseUrl: "https://secure.example.com/v1", allowInsecureHttp: false }),
    );
  });

  it("修改同源路径保留草稿密钥，换域或服务商后必须重新输入", async () => {
    render(<ModelConnectionsPanel />);
    await screen.findByRole("region", { name: "模型目录" });
    fireEvent.click(await addModelButton());
    const dialogEl = screen.getByRole("dialog", { name: "添加模型" });
    const dialog = within(dialogEl);
    chooseService(dialogEl, "自定义（OpenAI 兼容）");
    const address = dialog.getByLabelText("接口地址");
    const key = dialog.getByLabelText("API Key");
    fireEvent.change(address, { target: { value: "https://first.example/v1" } });
    fireEvent.change(key, { target: { value: "first-key" } });
    fireEvent.change(address, { target: { value: "https://first.example/compatible/v1" } });
    expect(key).toHaveValue("first-key");
    fireEvent.change(address, { target: { value: "https://second.example/v1" } });
    expect(key).toHaveValue("");
    fireEvent.click(dialog.getByRole("button", { name: "测试连接" }));
    expect(dialog.getByText("请填写 API 密钥。")).toBeInTheDocument();
    fireEvent.click(dialog.getByRole("button", { name: "保存模型" }));
    expect(mocks.providersTestConnection).not.toHaveBeenCalled();
    expect(mocks.providersSaveConnection).not.toHaveBeenCalled();
    fireEvent.change(key, { target: { value: "second-key" } });
    chooseService(dialogEl, "OpenAI");
    expect(dialog.getByLabelText("API Key")).toHaveValue("");
  });

  it("模型测试失败保留已保存的连接，不修改默认配置", async () => {
    mocks.providersList.mockResolvedValue({ providers: [importProvider], models: [{ modelId: "p/model", remoteModelId: "special-model", providerId: importProvider.id }] });
    mocks.providersTestModelConnection.mockRejectedValue(new Error("自定义网关需要额外参数"));
    render(<ModelConnectionsPanel />);
    await waitForModelList();
    fireEvent.click(screen.getByRole("button", { name: "测试模型 special-model" }));
    expect(await screen.findByRole("status")).toHaveTextContent("失败");
    fireEvent.click(screen.getByRole("button", { name: "查看 special-model 的测试错误" }));
    expect(screen.getByText("自定义网关需要额外参数")).toBeVisible();
    expect(screen.getByRole("button", { name: "添加模型" })).toBeEnabled();
    expect(mocks.providersSaveConnection).not.toHaveBeenCalled();
    expect(mocks.agentsDefaultsSave).not.toHaveBeenCalled();
  });

  it("删除模型后全局刷新失败时关闭确认框，避免重复删除", async () => {
    const model = { modelId: "personal/a", remoteModelId: "model-a", providerId: importProvider.id };
    mocks.providersList.mockResolvedValue({ providers: [importProvider], models: [model] });
    mocks.providersDeleteModel.mockImplementation(async () => {
      mocks.providersList.mockResolvedValue({ providers: [importProvider], models: [] });
    });
    const onModelsChanged = vi.fn().mockRejectedValue(new Error("shell refresh failed"));
    render(<ModelConnectionsPanel onModelsChanged={onModelsChanged} />);
    await waitForModelList();
    fireEvent.click(openMenu(screen, "模型操作 model-a").getByRole("menuitem", { name: "删除" }));
    const confirmation = screen.getByRole("alertdialog", { name: "删除模型“model-a”？" });
    fireEvent.click(within(confirmation).getByRole("button", { name: "删除模型" }));
    expect(await screen.findByText(/shell refresh failed/)).toHaveTextContent("配置已保存");
    expect(screen.queryByRole("alertdialog")).toBeNull();
    expect(mocks.providersDeleteModel).toHaveBeenCalledExactlyOnceWith(model.modelId);
  });

  it.each([false, true])("无组织上下文时 loggedIn=%s 模型页都不展示无关组织同步", async loggedIn => {
    mocks.orgSession.mockResolvedValue({ loggedIn, serverUrl: "https://account.test", bootstrap: { scopes: [{ id: "personal", kind: "personal", name: "个人" }] } });
    mocks.providersList.mockResolvedValue({ providers: [importProvider], models: [{ modelId: "a", providerId: importProvider.id }] });
    render(<ModelConnectionsPanel />);
    await waitForModelList();
    expect(screen.queryByText(/同步|组织模型|组织提供/)).toBeNull();
    expect(screen.getByRole("button", { name: "添加模型" })).toBeVisible();
    expect(screen.queryByRole("status", { name: "组织模型状态" })).toBeNull();
    expect(mocks.orgSyncModelConfig).not.toHaveBeenCalled();
  });

  it("组织空目录只有在同步明确返回空结果后才显示", async () => {
    mocks.orgSession.mockResolvedValue({ loggedIn: true, serverUrl: "https://org.test", bootstrap: { scopes: [{ id: "org", kind: "org", name: "组织" }] } });
    mocks.orgSyncModelConfig.mockResolvedValue({ configured: false, syncedAt: 1 });
    render(<ModelConnectionsPanel />);
    await waitForModelList();
    expect(screen.getByRole("status", { name: "组织模型状态" })).toHaveTextContent("尚未获取组织模型");
    expect(screen.queryByText("组织暂未提供模型")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "刷新组织模型" }));
    await waitFor(() => expect(screen.getByRole("status", { name: "组织模型状态" })).toHaveTextContent("组织暂未提供模型"));
    expect(mocks.orgSyncModelConfig).toHaveBeenCalledTimes(1);
    expect(mocks.agentsDefaultsSave).not.toHaveBeenCalled();
  });

  it("组织刷新失败不能伪装成未分配，个人模型仍可管理", async () => {
    mocks.orgSession.mockResolvedValue({ loggedIn: true, serverUrl: "https://org.test", bootstrap: { scopes: [{ id: "org", kind: "org", name: "组织" }] } });
    mocks.orgSyncModelConfig.mockRejectedValue(new Error("网络不可达"));
    mocks.providersList.mockResolvedValue({ providers: [importProvider], models: [{ modelId: "a", providerId: importProvider.id }] });
    render(<ModelConnectionsPanel />);
    await waitForModelList();
    fireEvent.click(screen.getByRole("button", { name: "刷新组织模型" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("暂时无法获取组织模型：网络不可达");
    expect(screen.queryByText("组织暂未提供模型")).toBeNull();
    expect(screen.getByRole("button", { name: "添加模型" })).toBeEnabled();
  });

  it.each([false, true])("组织认证过期时 loggedIn=%s 仍给出登录入口，个人模型不被禁用", async loggedIn => {
    const login = vi.fn();
    mocks.orgSession.mockResolvedValue({ loggedIn, requiresReauthentication: true, serverUrl: "https://org.test" });
    mocks.providersList.mockResolvedValue({ providers: [{ ...importProvider, id: "org", source: "organization", managed: true }, importProvider], models: [{ modelId: "org/a", providerId: "org" }, { modelId: "p/a", providerId: importProvider.id }] });
    render(<ModelConnectionsPanel onOpenOrganization={login} />);
    expect(await screen.findByRole("button", { name: "测试模型 org/a" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "测试模型 p/a" })).toBeEnabled();
    fireEvent.click(screen.getByRole("button", { name: "前往组织登录" }));
    expect(login).toHaveBeenCalledTimes(1);
    expect(mocks.orgSyncModelConfig).not.toHaveBeenCalled();
  });

  it("读取失败提供重试，不能显示空模型的假象", async () => {
    mocks.providersList.mockRejectedValueOnce(new Error("配置不可读"));
    render(<ModelConnectionsPanel />);
    expect(await screen.findByRole("alert")).toHaveTextContent("读取模型配置失败：配置不可读");
    expect(screen.queryByRole("region", { name: "模型目录" })).toBeNull();
    expect(screen.queryByRole("button", { name: "添加模型" })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "重新读取" }));
    expect(await screen.findByRole("region", { name: "模型目录" })).toBeInTheDocument();
    expect(mocks.providersList).toHaveBeenCalledTimes(2);
  });

  it.each(["添加模型", "编辑模型"] as const)("%s 保存从写入到全局刷新完成都持有操作锁", async title => {
    mocks.providersList.mockResolvedValue({ providers: [importProvider], models: [{ modelId: "a", remoteModelId: "remote-a", providerId: importProvider.id }] });
    let finishSave!: (value: { providerId: string; modelIds: string[] }) => void;
    let finishRefresh!: () => void;
    mocks.providersSaveConnection.mockImplementationOnce(() => new Promise(resolve => { finishSave = resolve; }));
    const changed = vi.fn(() => new Promise<void>(resolve => { finishRefresh = resolve; }));
    render(<ModelConnectionsPanel initialModelId="a" onModelsChanged={changed} />);
    const add = await addModelButton();
    fireEvent.click(title === "添加模型" ? add : screen.getByRole("button", { name: "编辑模型 remote-a" }));
    const dialog = screen.getByRole("dialog", { name: title });
    if (title === "添加模型") {
      chooseService(dialog, "工作连接");
      fireEvent.change(within(dialog).getByLabelText("模型名称"), { target: { value: "remote-b" } });
    }
    const save = within(dialog).getByRole("button", { name: "保存模型" });
    fireEvent.click(save);
    fireEvent.click(save);
    expect(mocks.providersSaveConnection).toHaveBeenCalledTimes(1);
    expect(add).toBeDisabled();
    expect(mocks.internalReload).not.toHaveBeenCalled();
    await act(async () => finishSave({ providerId: importProvider.id, modelIds: ["a"] }));
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(changed).toHaveBeenCalledTimes(1);
    expect(add).toBeDisabled();
    await act(async () => finishRefresh());
    await waitFor(() => expect(add).toBeEnabled());
    expect(mocks.internalReload).toHaveBeenCalledExactlyOnceWith("models");
    expect(mocks.providersSaveConnection).toHaveBeenCalledTimes(1);
  });

  it("服务发现只选择一个模型，切换接口时清除旧发现列表并保留手填模型", async () => {
    mocks.providersTestConnection.mockResolvedValue([{ id: "first" }, { id: "second" }]);
    render(<ModelConnectionsPanel />);
    fireEvent.click(await addModelButton());
    const dialog = screen.getByRole("dialog", { name: "添加模型" });
    chooseService(dialog, "DeepSeek");
    fireEvent.change(within(dialog).getByLabelText("API Key"), { target: { value: "test-key" } });
    fireEvent.click(within(dialog).getByRole("button", { name: "测试连接" }));
    fireEvent.click(await within(dialog).findByRole("radio", { name: "first" }));
    fireEvent.click(within(dialog).getByRole("radio", { name: "second" }));
    expect(within(dialog).getByRole("radio", { name: "first" })).not.toBeChecked();
    expect(within(dialog).getByLabelText("模型名称")).toHaveValue("second");
    fireEvent.change(within(dialog).getByLabelText("API Key"), { target: { value: "another-key" } });
    expect(within(dialog).queryByRole("radio")).toBeNull();
    expect(within(dialog).getByLabelText("模型名称")).toHaveValue("second");
    expect(mocks.providersSaveConnection).not.toHaveBeenCalled();
    expect(within(dialog).queryByText(/录音|语音额度/)).toBeNull();
  });

  it("保存已成功但加载失败时只重试加载，不重复保存", async () => {
    mocks.providersSaveConnection.mockResolvedValue({ providerId: "custom", modelIds: ["a"] });
    mocks.internalReload.mockRejectedValueOnce(new Error("运行时暂不可用")).mockResolvedValue(undefined);
    render(<ModelConnectionsPanel />);
    fireEvent.click(await addModelButton());
    const dialog = screen.getByRole("dialog", { name: "添加模型" });
    chooseService(dialog, "自定义（OpenAI 兼容）");
    fireEvent.change(within(dialog).getByLabelText("接口地址"), { target: { value: "https://test.example/v1" } });
    fireEvent.change(within(dialog).getByLabelText("API Key"), { target: { value: "test-key" } });
    fireEvent.change(within(dialog).getByLabelText("模型名称"), { target: { value: "a" } });
    fireEvent.click(within(dialog).getByRole("button", { name: "保存模型" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("配置已保存，但暂未生效");
    fireEvent.click(screen.getByRole("button", { name: "重试加载" }));
    await waitFor(() => expect(screen.getByRole("status")).toHaveTextContent("模型状态已刷新"));
    expect(mocks.providersSaveConnection).toHaveBeenCalledTimes(1);
    expect(mocks.providersTestModelConnection).not.toHaveBeenCalled();
  });

  it("组织身份切换时清除旧目录与未完成的旧测试结果", async () => {
    mocks.orgSession.mockResolvedValue({ loggedIn: true, serverUrl: "https://first-org.test" });
    mocks.providersList.mockResolvedValue({ providers: [{ ...importProvider, id: "first-org", source: "organization", managed: true }], models: [{ modelId: "first/model", remoteModelId: "first-model", providerId: "first-org" }] });
    let finishOldTest!: () => void;
    mocks.providersTestModelConnection.mockImplementationOnce(() => new Promise<void>(resolve => { finishOldTest = resolve; }));
    render(<ModelConnectionsPanel />);
    await waitForModelList();
    fireEvent.click(screen.getByRole("button", { name: "测试模型 first-model" }));
    mocks.providersList.mockResolvedValue({ providers: [importProvider], models: [{ modelId: "new/model", providerId: importProvider.id }] });
    act(() => useOrgSessionStore.getState().setSession({ loggedIn: true, serverUrl: "https://next-org.test" }));
    await screen.findByRole("button", { name: "测试模型 new/model" });
    expect(screen.queryByRole("button", { name: "测试模型 first-model" })).toBeNull();
    await act(async () => finishOldTest());
    expect(within(screen.getByRole("button", { name: "测试模型 new/model" }).closest("li")!).queryByRole("status")).toBeNull();
    expect(document.querySelector(".provider-card__test")).toBeNull();
  });

  it("空目录之后收到组织配置更新时不保留旧的空状态", async () => {
    let changed!: () => void;
    mocks.orgSession.mockResolvedValue({ loggedIn: true, serverUrl: "https://org.test", bootstrap: { scopes: [{ id: "org", kind: "org", name: "组织" }] } });
    mocks.listenOrgModelsChanged.mockImplementationOnce(async callback => { changed = callback; return () => {}; });
    mocks.orgSyncModelConfig.mockResolvedValue({ configured: false, syncedAt: 1 });
    render(<ModelConnectionsPanel />);
    await waitForModelList();
    fireEvent.click(screen.getByRole("button", { name: "刷新组织模型" }));
    await waitFor(() => expect(screen.getByRole("status", { name: "组织模型状态" })).toHaveTextContent("组织暂未提供模型"));
    mocks.providersList.mockResolvedValue({ providers: [{ ...importProvider, source: "organization", managed: true }], models: [{ modelId: "new-org-model", providerId: importProvider.id }] });
    act(() => changed());
    await screen.findByRole("button", { name: "测试模型 new-org-model" });
    expect(screen.getByRole("status", { name: "组织模型状态" })).not.toHaveTextContent("组织暂未提供模型");
  });

  it.each([
    ["kimi", "Kimi", "https://api.moonshot.cn/v1"],
    ["zhipu", "智谱 GLM", "https://open.bigmodel.cn/api/paas/v4/"],
  ])("%s 预设沿用已有兼容协议，保存并重新编辑时不丢失配置", async (service, label, baseUrl) => {
    mocks.providersSaveConnection.mockImplementation(async (provider, models) => {
      mocks.providersList.mockResolvedValue({ providers: [{ ...provider, id: "saved", apiKey: undefined, credentialConfigured: true, source: "personal" }], models: models.map((model: object) => ({ ...model, modelId: "saved/model", providerId: "saved" })) });
      return { providerId: "saved", modelIds: ["saved/model"] };
    });
    render(<ModelConnectionsPanel />);
    fireEvent.click(await addModelButton());
    const dialog = screen.getByRole("dialog", { name: "添加模型" });
    chooseService(dialog, { kimi: "Kimi（月之暗面）", zhipu: "智谱 GLM" }[service]!);
    fireEvent.change(within(dialog).getByLabelText("API Key"), { target: { value: "isolated-test-key" } });
    fireEvent.change(within(dialog).getByLabelText("模型名称"), { target: { value: "test-model" } });
    fireEvent.click(within(dialog).getByRole("button", { name: "保存模型" }));
    await screen.findByRole("region", { name: `${label}` });
    expect(mocks.providersSaveConnection).toHaveBeenCalledExactlyOnceWith(
      expect.objectContaining({ providerKind: "custom", apiBackend: "chat_completions", authScheme: "bearer", baseUrl }),
      [expect.objectContaining({ remoteModelId: "test-model" })], true,
    );
    expect(mocks.providersTestModelConnection).not.toHaveBeenCalled();
    expect(mocks.agentsDefaultsSave).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "编辑模型 test-model" }));
    expect(screen.getByLabelText("供应商")).toHaveValue(service);
    expect(screen.getByLabelText("API Key")).toHaveValue("");
    expect(screen.getByLabelText("模型名称")).toHaveValue("test-model");
  });

  it("先在表单中选择供应商，再填写密钥；密钥帮助不提交配置", async () => {
    mocks.openExternalUrl.mockResolvedValue(undefined);
    render(<ModelConnectionsPanel />);
    fireEvent.click(await addModelButton());
    const dialog = screen.getByRole("dialog", { name: "添加模型" });
    expect(within(dialog).queryByLabelText("API Key")).toBeNull();
    expect(within(dialog).getByRole("button", { name: "保存模型" })).toBeDisabled();
    expect(within(dialog).getByLabelText("供应商")).toHaveFocus();
    chooseService(dialog, "DeepSeek");
    expect(within(dialog).getByLabelText("供应商")).toHaveValue("deepseek");
    fireEvent.click(within(dialog).getByRole("button", { name: "获取密钥" }));
    expect(mocks.openExternalUrl).toHaveBeenCalledExactlyOnceWith("https://platform.deepseek.com");
    fireEvent.click(within(dialog).getByRole("button", { name: "保存模型" }));
    expect(within(dialog).getByLabelText("API Key")).toHaveAttribute("aria-invalid", "true");
    expect(within(dialog).getByLabelText("API Key")).toHaveFocus();
    expect(mocks.providersSaveConnection).not.toHaveBeenCalled();
  });

  it("更换服务商时同源密钥保留，跨源密钥清空", async () => {
    render(<ModelConnectionsPanel />);
    fireEvent.click(await addModelButton());
    const dialogEl = screen.getByRole("dialog", { name: "添加模型" });
    const dialog = within(dialogEl);
    chooseService(dialogEl, "自定义（OpenAI 兼容）");
    fireEvent.change(dialog.getByLabelText("接口地址"), { target: { value: "https://api.deepseek.com/compat" } });
    fireEvent.change(dialog.getByLabelText("API Key"), { target: { value: "same-origin-key" } });
    chooseService(dialogEl, "DeepSeek");
    expect(dialog.getByLabelText("API Key")).toHaveValue("same-origin-key");
    chooseService(dialogEl, "OpenAI");
    expect(dialog.getByLabelText("API Key")).toHaveValue("");
  });

  it("更换服务商后保留已填写的模型 ID", async () => {
    render(<ModelConnectionsPanel />);
    fireEvent.click(await addModelButton());
    const dialogEl = screen.getByRole("dialog", { name: "添加模型" });
    const dialog = within(dialogEl);
    chooseService(dialogEl, "DeepSeek");
    fireEvent.change(dialog.getByLabelText("模型名称"), { target: { value: "typed-model" } });
    chooseService(dialogEl, "Kimi（月之暗面）");
    expect(dialog.getByLabelText("供应商")).toHaveValue("kimi");
    expect(dialog.getByLabelText("模型名称")).toHaveValue("typed-model");
  });

  it("从复用连接改选服务商时清空复用时选择的模型", async () => {
    mocks.providersList.mockResolvedValue({ providers: [importProvider], models: [] });
    render(<ModelConnectionsPanel />);
    await waitForModelList();
    fireEvent.click(await addModelButton());
    const dialogEl = screen.getByRole("dialog", { name: "添加模型" });
    const dialog = within(dialogEl);
    chooseService(dialogEl, "工作连接");
    fireEvent.change(dialog.getByLabelText("模型名称"), { target: { value: "reused-model" } });
    chooseService(dialogEl, "DeepSeek");
    expect(dialog.getByLabelText("模型名称")).toHaveValue("");
  });

  it("更换服务商后重选同一服务时保留地址、HTTP 同意和密钥", async () => {
    render(<ModelConnectionsPanel />);
    fireEvent.click(await addModelButton());
    const dialogEl = screen.getByRole("dialog", { name: "添加模型" });
    const dialog = within(dialogEl);
    chooseService(dialogEl, "自定义（OpenAI 兼容）");
    fireEvent.change(dialog.getByLabelText("接口地址"), { target: { value: "http://gateway.example.test:8080/v1" } });
    fireEvent.click(dialog.getByLabelText(/允许此个人连接使用 HTTP/));
    fireEvent.change(dialog.getByLabelText("API Key"), { target: { value: "kept-key" } });
    chooseService(dialogEl, "自定义（OpenAI 兼容）");
    expect(dialog.getByLabelText("接口地址")).toHaveValue("http://gateway.example.test:8080/v1");
    expect(dialog.getByLabelText(/允许此个人连接使用 HTTP/)).toBeChecked();
    expect(dialog.getByLabelText("API Key")).toHaveValue("kept-key");
  });

  it("高级配置默认折叠；高级字段校验失败时自动展开并聚焦", async () => {
    render(<ModelConnectionsPanel />);
    fireEvent.click(await addModelButton());
    const dialogEl = screen.getByRole("dialog", { name: "添加模型" });
    const dialog = within(dialogEl);
    chooseService(dialogEl, "DeepSeek");
    const details = dialogEl.querySelector("details.model-connection-editor__advanced") as HTMLDetailsElement;
    expect(details.open).toBe(false);
    fireEvent.change(dialog.getByLabelText("API Key"), { target: { value: "k" } });
    fireEvent.change(dialog.getByLabelText("模型名称"), { target: { value: "deepseek-chat" } });
    openAdvanced(dialogEl);
    fireEvent.change(dialog.getByLabelText("输入，上下文窗口（tokens）"), { target: { value: "0" } });
    fireEvent.click(dialog.getByText("高级配置"));
    expect(details.open).toBe(false);
    fireEvent.click(dialog.getByRole("button", { name: "保存模型" }));
    expect(details.open).toBe(true);
    expect(dialog.getByText("上下文窗口必须是大于 0 的整数。")).toBeInTheDocument();
    expect(dialog.getByLabelText("输入，上下文窗口（tokens）")).toHaveAttribute("aria-invalid", "true");
    expect(dialog.getByLabelText("输入，上下文窗口（tokens）")).toHaveFocus();
    expect(mocks.providersSaveConnection).not.toHaveBeenCalled();
  });

  it("重新编辑预设模型时显示该连接已保存的自定义协议", async () => {
    mocks.providersList.mockResolvedValue({
      providers: [{ ...importProvider, label: "", providerKind: "deepseek", baseUrl: "https://api.deepseek.com", apiBackend: "responses" }],
      models: [{ modelId: "deepseek-chat", providerId: importProvider.id }],
    });
    render(<ModelConnectionsPanel />);
    fireEvent.click(await screen.findByRole("button", { name: "编辑模型 deepseek-chat" }));
    const editor = within(screen.getByRole("dialog", { name: "编辑模型" }));
    expect(editor.getByRole("checkbox", { name: "自定义协议" })).toBeChecked();
    expect(editor.getByLabelText("请求协议")).toHaveValue("responses");
  });

  it("组织登录过期时不能测试组织模型", async () => {
    mocks.orgSession.mockResolvedValue({ loggedIn: true, requiresReauthentication: true });
    mocks.providersList.mockResolvedValue({ providers: [{ ...importProvider, id: "org", source: "organization", managed: true, label: "团队" }], models: [{ modelId: "org/a", providerId: "org", name: "团队模型" }] });
    render(<ModelConnectionsPanel onOpenOrganization={() => {}} />);
    const card = await screen.findByRole("region", { name: "团队" });
    expect(within(card).getByRole("button", { name: "测试模型 团队模型" })).toBeDisabled();
  });

  it("未配置密钥可从模型行直接打开编辑模型", async () => {
    mocks.providersList.mockResolvedValue({
      providers: [{ ...importProvider, credentialConfigured: false }],
      models: [{ modelId: "model-a", providerId: importProvider.id }],
    });
    render(<ModelConnectionsPanel />);
    fireEvent.click(await screen.findByRole("button", { name: "未配置密钥" }));
    const dialog = within(screen.getByRole("dialog", { name: "编辑模型" }));
    expect(dialog.getByLabelText("API Key")).toHaveValue("");
  });

  it("模型目录支持搜索和直达，页面无独立服务入口", async () => {
    mocks.providersList.mockResolvedValue({
      providers: [importProvider, { ...importProvider, id: "org", source: "organization", managed: true, label: "团队" }],
      models: [
        { modelId: "personal/main", remoteModelId: "vendor-reasoner", providerId: importProvider.id, name: "工作模型" },
        { modelId: "org/main", providerId: "org", name: "团队模型" },
        ...Array.from({ length: 4 }, (_, i) => ({ modelId: `fixture-${i}`, providerId: importProvider.id })),
      ],
    });
    const view = render(<ModelConnectionsPanel />);
    const search = await screen.findByRole("searchbox", { name: "搜索模型" });
    expect(screen.getByRole("region", { name: "团队" })).toHaveTextContent("组织托管");
    fireEvent.change(search, { target: { value: "VENDOR-REASONER" } });
    expect(screen.getByText("工作模型")).toBeInTheDocument();
    expect(screen.queryByText("团队模型")).toBeNull();
    expect(screen.queryByRole("button", { name: "管理服务" })).toBeNull();
    expect(screen.queryByRole("region", { name: "服务连接" })).toBeNull();
    expect(screen.queryByRole("combobox", { name: "默认模型" })).toBeNull();

    view.rerender(<ModelConnectionsPanel initialModelId="org/main" />);
    const team = within(await screen.findByRole("region", { name: "团队" }));
    expect(team.getByRole("listitem")).toHaveFocus();
  });

  it("组织登录过期时组织分组显示需要重新登录", async () => {
    mocks.orgSession.mockResolvedValue({ loggedIn: true, requiresReauthentication: true });
    mocks.providersList.mockResolvedValue({ providers: [{ ...importProvider, id: "org", source: "organization", managed: true, label: "团队" }], models: [] });
    render(<ModelConnectionsPanel onOpenOrganization={() => {}} />);
    const status = await screen.findByRole("status", { name: "组织模型状态" });
    expect(status).toHaveTextContent("组织登录已过期，需要重新登录");
    expect(screen.getByRole("button", { name: "前往组织登录" })).toBeEnabled();
  });
});
