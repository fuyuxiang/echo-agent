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

async function openFirstConnection() {
  fireEvent.click(await screen.findByRole("button", { name: "管理模型连接" }));
  fireEvent.click(within(await screen.findByRole("group", { name: "模型连接" })).getAllByRole("button").find(button => button.classList.contains("model-settings__connection"))!);
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
    await openFirstConnection();
    expect(screen.getByText(/测试会验证一次简短文本回复.*图片和工具调用能力需另行验证/)).toBeVisible();
    const button = await screen.findByRole("button", { name: "测试模型 模型 B" });
    fireEvent.click(button);
    fireEvent.click(button);
    expect(button).toBeDisabled();
    expect(screen.getByRole("button", { name: "测试模型 模型 A" })).toBeEnabled();
    expect(mocks.providersTestModelConnection).toHaveBeenCalledExactlyOnceWith(expect.objectContaining({ id: provider.id, apiKey: undefined }), "remote-b");
    await act(async () => { rejectTest(new Error("模型接口返回 429：请稍后重试")); });
    const row = button.closest("li")!;
    expect(within(row).getByRole("status")).toHaveTextContent("上次测试失败");
    expect(within(row).getByRole("status")).toHaveTextContent("模型接口返回 429");
    fireEvent.click(button);
    expect(await within(row).findByRole("status")).toHaveTextContent("测试通过 · 耗时");
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
    render(<ModelConnectionsPanel initialModelId="personal/a" />);
    const region = await screen.findByRole("region", { name: "工作连接连接详情" });
    expect(within(region).getByRole("listitem")).toHaveAttribute("data-model-id", "personal/a");
    expect(within(region).getByRole("listitem")).toHaveFocus();
  });

  it("中文输入 Model ID 时 Enter 不添加，关闭草稿先确认", async () => {
    render(<ModelConnectionsPanel />);
    await screen.findByText("还没有可用模型");
    const opener = screen.getAllByRole("button", { name: "添加模型连接" })[0];
    opener.focus();
    fireEvent.click(opener);
    const dialog = screen.getByRole("dialog", { name: "添加模型连接" });
    fireEvent.change(within(dialog).getByLabelText("模型服务商"), { target: { value: "custom" } });
    const input = within(dialog).getByLabelText("模型 ID");
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

  it("同步模型读取失败可就地重试，等待时不重复读取或保存", async () => {
    mocks.providersList.mockResolvedValue({ providers: [importProvider], models: [] });
    let resolveModels!: (models: Array<{ id: string }>) => void;
    mocks.providersFetchModelsForProvider.mockReset()
      .mockRejectedValueOnce(new Error("连接中断"))
      .mockImplementationOnce(() => new Promise((resolve) => { resolveModels = resolve; }));
    mocks.providersSaveConnection.mockReset();
    render(<ModelConnectionsPanel />);
    await openFirstConnection();
    fireEvent.click(await screen.findByRole("button", { name: "获取更多模型" }));
    const dialog = screen.getByRole("dialog", { name: "添加更多模型" });
    expect(await within(dialog).findByRole("alert")).toHaveTextContent("读取模型列表失败：连接中断");
    expect(within(dialog).getByRole("button", { name: /^添加.*个模型$/ })).toBeDisabled();

    const retry = within(dialog).getByRole("button", { name: "重试获取模型" });
    fireEvent.click(retry);
    fireEvent.click(retry);
    expect(within(dialog).getByRole("status")).toHaveTextContent("正在获取模型列表");
    expect(mocks.providersFetchModelsForProvider).toHaveBeenCalledTimes(2);
    expect(mocks.providersSaveConnection).not.toHaveBeenCalled();

    await act(async () => { resolveModels([{ id: "model-a" }]); });
    expect(within(dialog).queryByRole("alert")).not.toBeInTheDocument();
    const model = within(dialog).getByRole("checkbox", { name: "model-a" });
    expect(model).toBeEnabled();
    fireEvent.click(model);
    expect(within(dialog).getByRole("button", { name: "添加 1 个模型" })).toBeEnabled();
  });

  it("同步模型保存失败保留列表和勾选，调整选择后可再保存且pending防重", async () => {
    mocks.providersList.mockResolvedValue({
      providers: [importProvider],
      models: [{ modelId: "personal/configured", remoteModelId: "configured", providerId: "personal" }],
    });
    mocks.providersFetchModelsForProvider.mockReset().mockResolvedValue([
      { id: "configured" }, { id: "model-a" }, { id: "model-b" },
    ]);
    let resolveSave!: (result: { providerId: string; modelIds: string[] }) => void;
    mocks.providersSaveConnection.mockReset()
      .mockRejectedValueOnce(new Error("暂时无法写入配置"))
      .mockImplementationOnce(() => new Promise((resolve) => { resolveSave = resolve; }));
    render(<ModelConnectionsPanel />);
    await openFirstConnection();
    fireEvent.click(await screen.findByRole("button", { name: "获取更多模型" }));
    const dialog = screen.getByRole("dialog", { name: "添加更多模型" });
    const firstModel = await within(dialog).findByRole("checkbox", { name: "model-a" });
    const secondModel = within(dialog).getByRole("checkbox", { name: "model-b" });
    const configured = within(dialog).getByRole("checkbox", { name: /configured/ });
    expect(configured).toBeChecked();
    expect(configured).toBeDisabled();
    fireEvent.click(firstModel);
    fireEvent.click(secondModel);
    fireEvent.click(within(dialog).getByRole("button", { name: "添加 2 个模型" }));

    expect(await within(dialog).findByRole("alert")).toHaveTextContent("保存模型失败：暂时无法写入配置");
    expect(firstModel).toBeChecked();
    expect(firstModel).toBeEnabled();
    expect(secondModel).toBeChecked();
    fireEvent.click(firstModel);
    expect(within(dialog).queryByRole("alert")).not.toBeInTheDocument();
    const retry = within(dialog).getByRole("button", { name: "添加 1 个模型" });
    fireEvent.click(retry);
    fireEvent.click(retry);
    expect(secondModel).toBeDisabled();
    expect(within(dialog).getByRole("button", { name: "取消" })).toBeDisabled();
    expect(mocks.providersSaveConnection).toHaveBeenCalledTimes(2);
    expect(mocks.providersSaveConnection).toHaveBeenLastCalledWith(
      expect.objectContaining({ id: "personal", apiKey: undefined }),
      [{ modelId: "", remoteModelId: "model-b", providerId: "personal" }],
    );

    await act(async () => { resolveSave({ providerId: "personal", modelIds: ["model-b"] }); });
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "添加更多模型" })).not.toBeInTheDocument());
    expect(await screen.findByText("已添加 1 个模型。")).toBeInTheDocument();
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
    await openFirstConnection();

    expect(await screen.findByText("组织托管")).toBeInTheDocument();
    expect(screen.getByText("由组织安全配置 · 不可查看")).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "组织提供", level: 3 })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "编辑连接" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "删除连接" })).not.toBeInTheDocument();
    expect(screen.getByText("远端模型 ID：MiniMax-M3")).toBeInTheDocument();
    expect(screen.queryByText(/此连接使用 HTTP 明文传输/)).not.toBeInTheDocument();
  });

  it("兼容旧后端数据时排除已移除的内置连接和五个模型", async () => {
    mocks.providersList.mockResolvedValue({
      providers: [{ id: "echoagent-ojlab", label: "内置模型", providerKind: "custom", source: "builtin", managed: true }],
      models: ["Echo Brook", "Echo Cedar", "Echo Grove", "Echo Reed", "Echo Willow"].map(name => ({
        modelId: `echoagent-ojlab/${name}`, providerId: "echoagent-ojlab", name,
      })),
    });
    render(<ModelConnectionsPanel />);
    expect(await screen.findByText("还没有可用模型")).toBeInTheDocument();
    expect(screen.queryByText("内置模型")).not.toBeInTheDocument();
    expect(screen.queryByText(/Echo Brook|Echo Cedar|Echo Grove|Echo Reed|Echo Willow/)).not.toBeInTheDocument();
    expect(screen.getAllByRole("button", { name: "添加模型连接" }).length).toBeGreaterThan(0);
  });

  it("首页不展开连接详情，管理页可返回且保留模型来源", async () => {
    mocks.providersList.mockResolvedValue({ providers: [importProvider], models: [{ modelId: "model-a", providerId: importProvider.id }] });
    render(<ModelConnectionsPanel />);
    expect(await screen.findByRole("heading", { name: "模型" })).toBeInTheDocument();
    expect(screen.queryByRole("region", { name: "工作连接连接详情" })).toBeNull();
    await openFirstConnection();
    expect(screen.getByRole("region", { name: "工作连接连接详情" })).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "返回连接管理" }));
    expect(screen.getByRole("heading", { name: "连接管理" })).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "返回模型设置" }));
    expect(screen.getByRole("heading", { name: "模型" })).toBeInTheDocument();
  });

  it("个人 HTTP 连接仍显示明文传输提醒", async () => {
    mocks.providersList.mockResolvedValue({
      providers: [{ id: "custom", providerKind: "custom", label: "个人连接", source: "personal", baseUrl: "http://example.com/v1" }],
      models: [{ modelId: "model-a", providerId: "custom" }],
    });
    render(<ModelConnectionsPanel />);
    await openFirstConnection();
    expect(await screen.findByText("此连接使用 HTTP 明文传输；API Key、提问和模型回复可能被网络路径上的其他人看到。")).toBeInTheDocument();
  });

  it("区分用户设定的默认模型与当前自动使用的模型", async () => {
    mocks.agentsDefaultsGet.mockResolvedValue({ ...defaults, defaultModel: "model-b" });
    mocks.agentAuthStatus.mockResolvedValue({ ready: true, defaultModelId: "model-a" });
    mocks.providersList.mockResolvedValue({
      providers: [{ id: "custom", providerKind: "custom", label: "工作", source: "personal", baseUrl: "https://example.com/v1" }],
      models: [
        { modelId: "model-a", providerId: "custom" },
        { modelId: "model-b", providerId: "custom" },
      ],
    });
    render(<ModelConnectionsPanel />);
    await openFirstConnection();
    const detail = await screen.findByRole("region", { name: "工作连接详情" });
    const rows = within(detail).getAllByRole("listitem");
    expect(within(rows[0]).getByText("当前自动使用")).toBeInTheDocument();
    expect(within(rows[0]).getByRole("button", { name: "设为默认" })).toBeInTheDocument();
    expect(within(rows[1]).getByText("已设默认（当前未使用）")).toBeInTheDocument();
  });

  it("设定与当前使用一致时只显示一个默认状态", async () => {
    mocks.agentsDefaultsGet.mockResolvedValue({ ...defaults, defaultModel: "model-a" });
    mocks.agentAuthStatus.mockResolvedValue({ ready: true, defaultModelId: "model-a" });
    mocks.providersList.mockResolvedValue({
      providers: [{ id: "custom", providerKind: "custom", source: "personal", baseUrl: "https://example.com/v1" }],
      models: [{ modelId: "model-a", providerId: "custom" }],
    });
    render(<ModelConnectionsPanel />);
    await openFirstConnection();
    expect(await screen.findByText("已设默认 · 当前使用")).toBeInTheDocument();
    expect(screen.queryByText("当前自动使用")).not.toBeInTheDocument();
  });

  it.each(["missing-model", "echoagent-ojlab/missing-model", "organization/missing-model"])("已设默认模型 %s 不在目录中时隐藏前缀并解释自动回退", async (defaultModel) => {
    mocks.agentsDefaultsGet.mockResolvedValue({ ...defaults, defaultModel });
    mocks.agentAuthStatus.mockResolvedValue({ ready: true, defaultModelId: "model-a" });
    mocks.providersList.mockResolvedValue({
      providers: [{ id: "custom", providerKind: "custom", source: "personal", baseUrl: "https://example.com/v1" }],
      models: [{ modelId: "model-a", providerId: "custom" }],
    });
    render(<ModelConnectionsPanel />);
    expect(await screen.findByText(/已设默认模型「missing-model」不在当前模型目录中/)).toBeInTheDocument();
    expect(screen.getByText(/已设默认模型「missing-model」不在当前模型目录中/)).not.toHaveTextContent(/echoagent-ojlab\/|organization\//);
    expect(screen.getByText(/当前自动使用「/)).toBeInTheDocument();
  });

  it("手动编辑模型可设置并清除最大输出", async () => {
    mocks.providersList.mockResolvedValue({
      providers: [{ id: "custom", providerKind: "custom", source: "personal", baseUrl: "https://example.com/v1", contextWindow: 16000 }],
      models: [{ modelId: "model-a", remoteModelId: "model-a", providerId: "custom", maxOutputTokens: 8192 }],
    });
    mocks.providersSaveConnection.mockResolvedValue({ providerId: "custom", modelIds: ["model-a"] });
    render(<ModelConnectionsPanel />);
    await openFirstConnection();
    await screen.findByText("远端模型 ID：model-a");
    fireEvent.click(screen.getByRole("button", { name: "编辑模型 model-a" }));
    const dialog = screen.getByRole("dialog", { name: "编辑模型" });
    const maxOutput = within(dialog).getByLabelText("最大输出 tokens（可选）");
    expect(maxOutput).toHaveValue(8192);
    fireEvent.change(maxOutput, { target: { value: "20000" } });
    fireEvent.click(within(dialog).getByRole("button", { name: "保存模型" }));
    expect(within(dialog).getByText("最大输出不能超过上下文窗口。")).toBeInTheDocument();
    expect(mocks.providersSaveConnection).not.toHaveBeenCalled();
    fireEvent.change(maxOutput, { target: { value: "" } });
    fireEvent.click(within(dialog).getByRole("button", { name: "保存模型" }));
    await waitFor(() => expect(mocks.providersSaveConnection).toHaveBeenCalledWith(
      expect.objectContaining({ id: "custom" }),
      [expect.objectContaining({ modelId: "model-a", clearMaxOutputTokens: true, maxOutputTokens: undefined })],
    ));
  });

  it("手动新增模型可设置最大输出", async () => {
    mocks.providersList.mockResolvedValue({
      providers: [{ id: "custom", providerKind: "custom", source: "personal", baseUrl: "https://example.com/v1", contextWindow: 16000 }],
      models: [],
    });
    mocks.providersSaveConnection.mockResolvedValue({ providerId: "custom", modelIds: ["model-a"] });
    render(<ModelConnectionsPanel />);
    await openFirstConnection();
    await screen.findByRole("region", { name: "OpenAI 兼容连接详情" });
    fireEvent.click(screen.getByRole("button", { name: /手动添加/ }));
    const dialog = screen.getByRole("dialog", { name: "手动添加模型" });
    fireEvent.change(within(dialog).getByPlaceholderText("例如 MiniMax-M3、gpt-5"), { target: { value: "model-a" } });
    fireEvent.change(within(dialog).getByLabelText("最大输出 tokens（可选）"), { target: { value: "4096" } });
    fireEvent.click(within(dialog).getByRole("button", { name: "保存模型" }));
    await waitFor(() => expect(mocks.providersSaveConnection).toHaveBeenCalledWith(
      expect.objectContaining({ id: "custom" }),
      [expect.objectContaining({ remoteModelId: "model-a", maxOutputTokens: 4096, clearMaxOutputTokens: false })],
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
    await openFirstConnection();

    const detail = await screen.findByRole("region", { name: "工作用连接详情" });
    expect(within(detail).getByText("7 个")).toBeInTheDocument();
    expect(within(detail).getAllByRole("listitem")).toHaveLength(7);
    expect(within(detail).getByText("远端模型 ID：model-7")).toBeInTheDocument();
  });

  it("新增表单只保存配置，不发送实际模型请求", async () => {
    mocks.providersTestModelConnection.mockResolvedValue(undefined);
    mocks.providersSaveConnection.mockResolvedValue({
      providerId: "custom",
      modelIds: ["MiniMax-M3"],
    });

    render(<ModelConnectionsPanel />);
    await screen.findByText("还没有可用模型");
    fireEvent.click(screen.getAllByRole("button", { name: "添加模型连接" })[0]);

    const dialog = screen.getByRole("dialog", { name: "添加模型连接" });
    fireEvent.change(within(dialog).getByLabelText("模型服务商"), { target: { value: "custom" } });
    fireEvent.click(within(dialog).getByRole("button", { name: "更多设置" }));
    fireEvent.change(within(dialog).getByLabelText(/连接名称/), { target: { value: "我的 MiniMax" } });
    fireEvent.change(within(dialog).getByLabelText("服务地址（Base URL）"), { target: { value: "https://api.minimaxi.com/v1" } });
    fireEvent.change(within(dialog).getByLabelText("API 密钥"), { target: { value: "sk-test" } });
    fireEvent.change(within(dialog).getByLabelText("模型 ID"), { target: { value: "MiniMax-M3" } });
    expect(within(dialog).queryByRole("button", { name: "测试此模型" })).toBeNull();
    expect(within(dialog).queryByRole("button", { name: "添加更多模型" })).toBeNull();
    fireEvent.click(within(dialog).getByRole("button", { name: "保存" }));
    await waitFor(() => {
      expect(mocks.providersTestModelConnection).not.toHaveBeenCalled();
      expect(mocks.providersSaveConnection).toHaveBeenCalledWith(
        expect.objectContaining({
          id: "",
          label: "我的 MiniMax",
          baseUrl: "https://api.minimaxi.com/v1",
          apiKey: "sk-test",
          providerKind: "custom",
        }),
        [expect.objectContaining({ modelId: "", remoteModelId: "MiniMax-M3" })],
        true,
      );
    });
    expect(mocks.internalReload).toHaveBeenCalledWith("models");
  });

  it("原内置服务地址作为个人连接时也必须明确配置凭据", async () => {
    render(<ModelConnectionsPanel />);
    await screen.findByText("还没有可用模型");
    fireEvent.click(screen.getAllByRole("button", { name: "添加模型连接" })[0]);
    const dialog = screen.getByRole("dialog", { name: "添加模型连接" });
    fireEvent.change(within(dialog).getByLabelText("模型服务商"), { target: { value: "custom" } });
    fireEvent.change(within(dialog).getByLabelText("服务地址（Base URL）"), { target: { value: "http://123.56.188.16:8088/v1" } });
    fireEvent.change(within(dialog).getByLabelText("模型 ID"), { target: { value: "my-model" } });
    expect(within(dialog).getByLabelText("API 密钥")).toBeEnabled();
    fireEvent.click(within(dialog).getByLabelText(/允许此个人连接使用 HTTP/));
    fireEvent.click(within(dialog).getByRole("button", { name: "保存" }));
    expect(await within(dialog).findByText("请填写 API 密钥。")).toBeInTheDocument();
    expect(mocks.providersSaveConnection).not.toHaveBeenCalled();
  });

  it("OpenAI 作为 Responses 模型 Provider 接入而不是独立 Agent Runtime", async () => {
    render(<ModelConnectionsPanel />);
    await screen.findByText("还没有可用模型");
    fireEvent.click(screen.getAllByRole("button", { name: "添加模型连接" })[0]);

    const dialog = screen.getByRole("dialog", { name: "添加模型连接" });
    fireEvent.change(within(dialog).getByLabelText("模型服务商"), { target: { value: "custom" } });
    fireEvent.change(within(dialog).getByLabelText("模型服务商"), {
      target: { value: "openai" },
    });

    expect(within(dialog).getByText("服务地址：https://api.openai.com/v1")).toBeInTheDocument();
    fireEvent.click(within(dialog).getByRole("button", { name: "修改地址" }));
    expect(within(dialog).getByLabelText("服务地址（Base URL）")).toHaveValue("https://api.openai.com/v1");
    fireEvent.click(within(dialog).getByRole("button", { name: "更多设置" }));
    expect(within(dialog).getByDisplayValue("Responses")).toBeInTheDocument();
  });

  it("连接编辑器具有初始焦点、Tab 圈定、Escape 与触发器恢复", async () => {
    render(<ModelConnectionsPanel />);
    await screen.findByText("还没有可用模型");
    const opener = screen.getAllByRole("button", { name: "添加模型连接" })[0];
    opener.focus();
    fireEvent.click(opener);

    const dialog = screen.getByRole("dialog", { name: "添加模型连接" });
    expect(within(dialog).getByLabelText("模型服务商")).toHaveFocus();
    const last = within(dialog).getByRole("button", { name: "保存" });
    last.focus();
    fireEvent.keyDown(document, { key: "Tab" });
    expect(within(dialog).getByRole("button", { name: "关闭" })).toHaveFocus();

    fireEvent.keyDown(document, { key: "Escape" });
    expect(screen.queryByRole("dialog", { name: "添加模型连接" })).toBeNull();
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
    await screen.findByText("还没有可用模型");
    fireEvent.click(screen.getAllByRole("button", { name: "添加模型连接" })[0]);
    const dialog = screen.getByRole("dialog", { name: "添加模型连接" });
    fireEvent.change(within(dialog).getByLabelText("模型服务商"), { target: { value: "custom" } });
    fireEvent.change(within(dialog).getByLabelText("服务地址（Base URL）"), { target: { value: "https://api.example.com/v1" } });
    fireEvent.change(within(dialog).getByLabelText("API 密钥"), { target: { value: "sk-test" } });
    fireEvent.change(within(dialog).getByLabelText("模型 ID"), { target: { value: "model-a" } });
    fireEvent.click(within(dialog).getByRole("button", { name: "保存" }));

    await waitFor(() => expect(mocks.internalReload).toHaveBeenCalledWith("models"));
    expect(onModelsChanged).not.toHaveBeenCalled();

    await act(async () => resolveReload());
    await waitFor(() => expect(onModelsChanged).toHaveBeenCalledTimes(1));
    expect(screen.queryByText("连接已添加，已配置 1 个模型。")).not.toBeInTheDocument();

    await act(async () => resolveRefresh());
    expect(await screen.findByText("连接已添加，已配置 1 个模型。")).toBeInTheDocument();
  });

  it("编辑连接时留空密钥并由原生层复用已保存凭据", async () => {
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
    await openFirstConnection();
    await screen.findByText("现有连接", { selector: "h3" });
    fireEvent.click(screen.getByRole("button", { name: "编辑连接" }));
    const dialog = screen.getByRole("dialog", { name: "编辑连接" });
    expect(within(dialog).getByLabelText("API 密钥")).toHaveValue("");
    fireEvent.click(within(dialog).getByRole("button", { name: "保存" }));
    await waitFor(() => expect(mocks.providersSaveConnection).toHaveBeenCalledWith(
      expect.objectContaining({ id: "custom", apiKey: undefined, source: "personal" }),
      [expect.objectContaining({ modelId: "model-a", remoteModelId: "model-a" })], true,
    ));
    expect(mocks.providersTestModelConnection).not.toHaveBeenCalled();
  });

  it("编辑连接时完整保留已有多模型及其能力配置", async () => {
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
      models: [
        { modelId: "model-a", remoteModelId: "model-a", providerId: "custom", maxOutputTokens: 8192 },
        { modelId: "model-b", remoteModelId: "model-b", providerId: "custom" },
      ],
    });
    mocks.providersTestConnection.mockResolvedValue([{ id: "model-a" }, { id: "model-b" }]);
    mocks.providersSaveConnection.mockResolvedValue({ providerId: "custom", modelIds: ["model-a"] });

    render(<ModelConnectionsPanel />);
    await openFirstConnection();
    await screen.findByText("现有连接", { selector: "h3" });
    fireEvent.click(screen.getByRole("button", { name: "编辑连接" }));
    const dialog = screen.getByRole("dialog", { name: "编辑连接" });
    expect(within(dialog).getByLabelText("已配置模型")).toHaveTextContent("model-amodel-b");
    expect(within(dialog).queryByRole("button", { name: "从服务获取模型" })).toBeNull();
    fireEvent.click(within(dialog).getByRole("button", { name: "保存" }));
    await waitFor(() => expect(mocks.providersSaveConnection).toHaveBeenCalledWith(
      expect.objectContaining({ id: "custom", apiKey: undefined }),
      [expect.objectContaining({ modelId: "model-a", remoteModelId: "model-a", maxOutputTokens: 8192 }), expect.objectContaining({ modelId: "model-b", remoteModelId: "model-b" })], true,
    ));
  });

  it("服务端不支持 models 接口时仍可手填 Model ID 并保存", async () => {
    mocks.providersTestConnection.mockRejectedValue(new Error("404 /models"));
    mocks.providersSaveConnection.mockResolvedValue({
      providerId: "custom",
      modelIds: ["private-model-v2"],
    });

    render(<ModelConnectionsPanel />);
    await screen.findByText("还没有可用模型");
    fireEvent.click(screen.getAllByRole("button", { name: "添加模型连接" })[0]);
    const dialog = screen.getByRole("dialog", { name: "添加模型连接" });
    fireEvent.change(within(dialog).getByLabelText("模型服务商"), { target: { value: "custom" } });
    fireEvent.change(within(dialog).getByLabelText("服务地址（Base URL）"), { target: { value: "https://private.example/v1" } });
    fireEvent.change(within(dialog).getByLabelText("API 密钥"), { target: { value: "private-key" } });
    fireEvent.change(within(dialog).getByLabelText("模型 ID"), { target: { value: "private-model-v2" } });

    fireEvent.click(within(dialog).getByRole("button", { name: "从服务获取模型" }));
    expect(await within(dialog).findByText(/可手动填写模型 ID 后保存/)).toBeInTheDocument();
    fireEvent.click(within(dialog).getByRole("button", { name: "保存" }));

    await waitFor(() => {
      expect(mocks.providersSaveConnection).toHaveBeenCalledWith(
        expect.objectContaining({
          label: "OpenAI 兼容",
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
    await screen.findByText("还没有可用模型");
    fireEvent.click(screen.getAllByRole("button", { name: "添加模型连接" })[0]);
    const dialog = screen.getByRole("dialog", { name: "添加模型连接" });
    fireEvent.change(within(dialog).getByLabelText("模型服务商"), { target: { value: "custom" } });
    const baseUrl = within(dialog).getByLabelText("服务地址（Base URL）");
    fireEvent.change(baseUrl, { target: { value: "http://example.com/:60100/v1" } });
    fireEvent.change(within(dialog).getByLabelText("API 密钥"), { target: { value: "test-key" } });
    fireEvent.click(within(dialog).getByRole("button", { name: "从服务获取模型" }));
    expect(within(dialog).getByText(/端口号位置不对/)).toBeInTheDocument();
    expect(mocks.providersTestConnection).not.toHaveBeenCalled();

    fireEvent.change(baseUrl, { target: { value: "http://example.com:60100/v1" } });
    fireEvent.click(within(dialog).getByRole("button", { name: "从服务获取模型" }));
    expect(within(dialog).getByText(/使用 HTTP 前，请确认/)).toBeInTheDocument();
    expect(mocks.providersTestConnection).not.toHaveBeenCalled();

    fireEvent.click(within(dialog).getByLabelText(/允许此个人连接使用 HTTP/));
    fireEvent.change(within(dialog).getByLabelText("API 密钥"), { target: { value: "test-key" } });
    fireEvent.click(within(dialog).getByRole("button", { name: "从服务获取模型" }));
    await waitFor(() => expect(mocks.providersTestConnection).toHaveBeenCalledWith(
      expect.objectContaining({ baseUrl: "http://example.com:60100/v1", allowInsecureHttp: true }),
    ));

    fireEvent.change(baseUrl, { target: { value: "http://123.56.188.16:8088/v1" } });
    fireEvent.click(within(dialog).getByRole("button", { name: "从服务获取模型" }));
    expect(within(dialog).getByText(/使用 HTTP 前，请确认/)).toBeInTheDocument();
    expect(mocks.providersTestConnection).toHaveBeenCalledTimes(1);
    fireEvent.click(within(dialog).getByLabelText(/允许此个人连接使用 HTTP/));
    fireEvent.change(within(dialog).getByLabelText("API 密钥"), { target: { value: "test-key" } });
    fireEvent.click(within(dialog).getByRole("button", { name: "从服务获取模型" }));
    await waitFor(() => expect(mocks.providersTestConnection).toHaveBeenCalledTimes(2));
    expect(mocks.providersTestConnection).toHaveBeenLastCalledWith(
      expect.objectContaining({ baseUrl: "http://123.56.188.16:8088/v1", allowInsecureHttp: true }),
    );

    fireEvent.change(baseUrl, { target: { value: "https://secure.example.com/v1" } });
    fireEvent.change(within(dialog).getByLabelText("API 密钥"), { target: { value: "test-key" } });
    expect(within(dialog).queryByLabelText(/允许此个人连接使用 HTTP/)).not.toBeInTheDocument();
    fireEvent.click(within(dialog).getByRole("button", { name: "从服务获取模型" }));
    await waitFor(() => expect(mocks.providersTestConnection).toHaveBeenCalledTimes(3));
    expect(mocks.providersTestConnection).toHaveBeenLastCalledWith(
      expect.objectContaining({ baseUrl: "https://secure.example.com/v1", allowInsecureHttp: false }),
    );
  });

  it("修改同源路径保留草稿密钥，换域或服务商后必须重新输入", async () => {
    render(<ModelConnectionsPanel />);
    await screen.findByText("还没有可用模型");
    fireEvent.click(screen.getAllByRole("button", { name: "添加模型连接" })[0]);
    const dialog = within(screen.getByRole("dialog", { name: "添加模型连接" }));
    const service = dialog.getByLabelText("模型服务商");
    fireEvent.change(service, { target: { value: "custom" } });
    const address = dialog.getByLabelText("服务地址（Base URL）");
    const key = dialog.getByLabelText("API 密钥");
    fireEvent.change(address, { target: { value: "https://first.example/v1" } });
    fireEvent.change(key, { target: { value: "first-key" } });
    fireEvent.change(address, { target: { value: "https://first.example/compatible/v1" } });
    expect(key).toHaveValue("first-key");
    fireEvent.change(address, { target: { value: "https://second.example/v1" } });
    expect(key).toHaveValue("");
    fireEvent.click(dialog.getByRole("button", { name: "从服务获取模型" }));
    expect(dialog.getByText("请填写 API 密钥。")).toBeInTheDocument();
    fireEvent.click(dialog.getByRole("button", { name: "保存" }));
    expect(mocks.providersTestConnection).not.toHaveBeenCalled();
    expect(mocks.providersSaveConnection).not.toHaveBeenCalled();
    fireEvent.change(key, { target: { value: "second-key" } });
    fireEvent.change(service, { target: { value: "openai" } });
    expect(key).toHaveValue("");
  });

  it("模型测试失败保留已保存的连接，不修改默认配置", async () => {
    mocks.providersList.mockResolvedValue({ providers: [importProvider], models: [{ modelId: "p/model", remoteModelId: "special-model", providerId: importProvider.id }] });
    mocks.providersTestModelConnection.mockRejectedValue(new Error("自定义网关需要额外参数"));
    render(<ModelConnectionsPanel />);
    await openFirstConnection();
    fireEvent.click(screen.getByRole("button", { name: "测试模型 special-model" }));
    expect(await screen.findByRole("status")).toHaveTextContent("自定义网关需要额外参数");
    expect(screen.getByRole("button", { name: "编辑连接" })).toBeEnabled();
    expect(mocks.providersSaveConnection).not.toHaveBeenCalled();
    expect(mocks.agentsDefaultsSave).not.toHaveBeenCalled();
  });

  it("删除已成功但后续全局刷新失败时不保留可重复删除的弹窗", async () => {
    const provider = {
      id: "personal",
      providerKind: "custom" as const,
      label: "个人连接",
      source: "personal" as const,
      credentialConfigured: true,
      baseUrl: "https://api.example.com/v1",
      apiBackend: "chat_completions" as const,
      authScheme: "bearer" as const,
    };
    let deleted = false;
    mocks.providersList.mockImplementation(async () => deleted
      ? { providers: [], models: [] }
      : { providers: [provider], models: [] });
    mocks.providersDeleteProvider.mockImplementation(async () => { deleted = true; });
    const onModelsChanged = vi.fn().mockRejectedValue(new Error("shell refresh failed"));

    render(<ModelConnectionsPanel onModelsChanged={onModelsChanged} />);
    await openFirstConnection();
    await screen.findByText("个人连接", { selector: "h3" });
    fireEvent.click(screen.getByRole("button", { name: "删除连接" }));
    const dialog = screen.getByRole("alertdialog", { name: "删除连接“个人连接”？" });
    fireEvent.click(within(dialog).getByRole("button", { name: "删除连接" }));

    await waitFor(() => expect(mocks.providersDeleteProvider).toHaveBeenCalledTimes(1));
    await waitFor(() => expect(screen.queryByRole("alertdialog")).toBeNull());
    expect(await screen.findByText(/shell refresh failed/)).toHaveTextContent("配置已保存");
    expect(mocks.providersDeleteProvider).toHaveBeenCalledTimes(1);
  });
  it.each([false, true])("无组织上下文时 loggedIn=%s 首页与管理页都不展示无关组织同步", async loggedIn => {
    mocks.orgSession.mockResolvedValue({ loggedIn, serverUrl: "https://account.test", bootstrap: { scopes: [{ id: "personal", kind: "personal", name: "个人" }] } });
    mocks.providersList.mockResolvedValue({ providers: [importProvider], models: [{ modelId: "a", providerId: importProvider.id }] });
    render(<ModelConnectionsPanel />);
    await screen.findByRole("button", { name: "管理模型连接" });
    expect(screen.queryByText(/同步|组织模型|组织提供/)).toBeNull();
    expect(screen.queryByRole("button", { name: "添加模型连接" })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "管理模型连接" }));
    expect(screen.queryByRole("region", { name: "组织模型状态" })).toBeNull();
    expect(mocks.orgSyncModelConfig).not.toHaveBeenCalled();
  });

  it("组织空目录只有在同步明确返回空结果后才显示", async () => {
    mocks.orgSession.mockResolvedValue({ loggedIn: true, serverUrl: "https://org.test", bootstrap: { scopes: [{ id: "org", kind: "org", name: "组织" }] } });
    mocks.orgSyncModelConfig.mockResolvedValue({ configured: false, syncedAt: 1 });
    render(<ModelConnectionsPanel />);
    fireEvent.click(await screen.findByRole("button", { name: "管理模型连接" }));
    expect(screen.getByRole("region", { name: "组织模型状态" })).toHaveTextContent("尚未获取组织模型");
    expect(screen.queryByText("组织暂未提供模型")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "刷新组织模型" }));
    await waitFor(() => expect(screen.getByRole("region", { name: "组织模型状态" })).toHaveTextContent("组织暂未提供模型"));
    expect(mocks.orgSyncModelConfig).toHaveBeenCalledTimes(1);
    expect(mocks.agentsDefaultsSave).not.toHaveBeenCalled();
  });

  it("组织刷新失败不能伪装成未分配，个人模型仍可管理", async () => {
    mocks.orgSession.mockResolvedValue({ loggedIn: true, serverUrl: "https://org.test", bootstrap: { scopes: [{ id: "org", kind: "org", name: "组织" }] } });
    mocks.orgSyncModelConfig.mockRejectedValue(new Error("网络不可达"));
    mocks.providersList.mockResolvedValue({ providers: [importProvider], models: [{ modelId: "a", providerId: importProvider.id }] });
    render(<ModelConnectionsPanel />);
    fireEvent.click(await screen.findByRole("button", { name: "管理模型连接" }));
    fireEvent.click(screen.getByRole("button", { name: "刷新组织模型" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("暂时无法获取组织模型：网络不可达");
    expect(screen.queryByText("组织暂未提供模型")).toBeNull();
    fireEvent.click(within(screen.getByRole("group", { name: "模型连接" })).getByRole("button", { name: /工作连接/ }));
    expect(screen.getByRole("button", { name: "编辑连接" })).toBeEnabled();
  });

  it.each([false, true])("组织认证过期时 loggedIn=%s 仍给出登录入口，个人模型不被禁用", async loggedIn => {
    const login = vi.fn();
    mocks.orgSession.mockResolvedValue({ loggedIn, requiresReauthentication: true, serverUrl: "https://org.test" });
    mocks.providersList.mockResolvedValue({ providers: [{ ...importProvider, id: "org", source: "organization", managed: true }, importProvider], models: [{ modelId: "org/a", providerId: "org" }, { modelId: "p/a", providerId: importProvider.id }] });
    render(<ModelConnectionsPanel onOpenOrganization={login} />);
    const selector = await screen.findByRole("combobox", { name: "默认模型" });
    expect(within(selector).getByRole("option", { name: /org\/a/ })).toBeDisabled();
    expect(within(selector).getByRole("option", { name: /p\/a/ })).toBeEnabled();
    fireEvent.click(screen.getByRole("button", { name: "管理模型连接" }));
    fireEvent.click(screen.getByRole("button", { name: "前往组织登录" }));
    expect(login).toHaveBeenCalledTimes(1);
    expect(mocks.orgSyncModelConfig).not.toHaveBeenCalled();
  });

  it("读取失败提供重试，不能显示空模型的假象", async () => {
    mocks.providersList.mockRejectedValueOnce(new Error("配置不可读"));
    render(<ModelConnectionsPanel />);
    expect(await screen.findByRole("alert")).toHaveTextContent("读取模型配置失败：配置不可读");
    expect(screen.queryByText("还没有可用模型")).toBeNull();
    expect(screen.queryByRole("button", { name: "添加模型连接" })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "重新读取" }));
    expect(await screen.findByText("还没有可用模型")).toBeInTheDocument();
    expect(mocks.providersList).toHaveBeenCalledTimes(2);
  });

  it("设为默认保留其他默认配置，并沿用原有加载与刷新链路", async () => {
    mocks.providersList.mockResolvedValue({ providers: [importProvider], models: [{ modelId: "a", providerId: importProvider.id }] });
    const changed = vi.fn();
    render(<ModelConnectionsPanel onModelsChanged={changed} />);
    fireEvent.change(await screen.findByRole("combobox", { name: "默认模型" }), { target: { value: "a" } });
    await waitFor(() => expect(changed).toHaveBeenCalledTimes(1));
    expect(mocks.agentsDefaultsSave).toHaveBeenCalledExactlyOnceWith({ ...defaults, defaultModel: "a" });
    expect(mocks.internalReload).toHaveBeenCalledWith("models");
  });

  it("设为默认未完成时禁用所有编辑入口，完成后可正常保存连接", async () => {
    mocks.providersList.mockResolvedValue({ providers: [importProvider], models: [{ modelId: "a", remoteModelId: "remote-a", providerId: importProvider.id }] });
    let finishDefault!: () => void;
    mocks.agentsDefaultsSave.mockImplementationOnce(() => new Promise<void>(resolve => { finishDefault = resolve; }));
    mocks.providersSaveConnection.mockResolvedValue({ providerId: importProvider.id, modelIds: ["a"] });
    const changed = vi.fn();
    render(<ModelConnectionsPanel initialModelId="a" onModelsChanged={changed} />);
    const detail = await screen.findByRole("region", { name: "工作连接连接详情" });
    fireEvent.click(within(detail).getByRole("button", { name: "设为默认" }));
    for (const name of ["编辑连接", "删除连接", "手动添加", "获取更多模型", "编辑模型 remote-a", "删除模型 remote-a"]) {
      const button = within(detail).getByRole("button", { name });
      expect(button).toBeDisabled();
      fireEvent.click(button);
    }
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(mocks.providersSaveConnection).not.toHaveBeenCalled();
    await act(async () => finishDefault());
    await waitFor(() => expect(within(detail).getByRole("button", { name: "编辑连接" })).toBeEnabled());
    fireEvent.click(within(detail).getByRole("button", { name: "编辑连接" }));
    fireEvent.click(within(screen.getByRole("dialog", { name: "编辑连接" })).getByRole("button", { name: "保存" }));
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "编辑连接" })).toBeNull());
    await waitFor(() => expect(changed).toHaveBeenCalledTimes(2));
    expect(mocks.providersSaveConnection).toHaveBeenCalledTimes(1);
  });

  it.each(["connection", "manual", "import"] as const)("%s 保存从写入到全局刷新完成都持有操作锁", async editor => {
    mocks.providersList.mockResolvedValue({ providers: [importProvider], models: [{ modelId: "a", remoteModelId: "remote-a", providerId: importProvider.id }] });
    mocks.providersFetchModelsForProvider.mockResolvedValue([{ id: "remote-b" }]);
    let finishSave!: (value: { providerId: string; modelIds: string[] }) => void;
    let finishRefresh!: () => void;
    mocks.providersSaveConnection.mockImplementationOnce(() => new Promise(resolve => { finishSave = resolve; }));
    const changed = vi.fn(() => new Promise<void>(resolve => { finishRefresh = resolve; }));
    render(<ModelConnectionsPanel initialModelId="a" onModelsChanged={changed} />);
    const detail = await screen.findByRole("region", { name: "工作连接连接详情" });
    const entry = { connection: "编辑连接", manual: "手动添加", import: "获取更多模型" }[editor]!;
    fireEvent.click(within(detail).getByRole("button", { name: entry }));
    const dialog = screen.getByRole("dialog");
    if (editor === "manual") fireEvent.change(within(dialog).getByLabelText("远端模型 ID"), { target: { value: "remote-b" } });
    if (editor === "import") fireEvent.click(await within(dialog).findByRole("checkbox", { name: "remote-b" }));
    const saveLabel = { connection: "保存", manual: "保存模型", import: "添加 1 个模型" }[editor]!;
    const save = within(dialog).getByRole("button", { name: saveLabel });
    fireEvent.click(save); fireEvent.click(save);
    expect(mocks.providersSaveConnection).toHaveBeenCalledTimes(1);
    expect(within(detail).getByRole("button", { name: "编辑连接" })).toBeDisabled();
    expect(mocks.internalReload).not.toHaveBeenCalled();
    await act(async () => finishSave({ providerId: importProvider.id, modelIds: ["a"] }));
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(changed).toHaveBeenCalledTimes(1);
    expect(within(detail).getByRole("button", { name: "编辑连接" })).toBeDisabled();
    await act(async () => finishRefresh());
    await waitFor(() => expect(within(detail).getByRole("button", { name: "编辑连接" })).toBeEnabled());
    expect(mocks.internalReload).toHaveBeenCalledExactlyOnceWith("models");
    expect(mocks.providersSaveConnection).toHaveBeenCalledTimes(1);
  });

  it("服务发现只选择一个模型，切换接口时清除旧发现列表并保留手填模型", async () => {
    mocks.providersTestConnection.mockResolvedValue([{ id: "first" }, { id: "second" }]);
    render(<ModelConnectionsPanel />);
    fireEvent.click(await screen.findByRole("button", { name: "添加模型连接" }));
    const dialog = screen.getByRole("dialog", { name: "添加模型连接" });
    fireEvent.change(within(dialog).getByLabelText("模型服务商"), { target: { value: "deepseek" } });
    fireEvent.change(within(dialog).getByLabelText("API 密钥"), { target: { value: "test-key" } });
    fireEvent.click(within(dialog).getByRole("button", { name: "从服务获取模型" }));
    fireEvent.click(await within(dialog).findByRole("radio", { name: "first" }));
    fireEvent.click(within(dialog).getByRole("radio", { name: "second" }));
    expect(within(dialog).getByRole("radio", { name: "first" })).not.toBeChecked();
    expect(within(dialog).getByLabelText("模型 ID")).toHaveValue("second");
    fireEvent.change(within(dialog).getByLabelText("API 密钥"), { target: { value: "another-key" } });
    expect(within(dialog).queryByRole("radio")).toBeNull();
    expect(within(dialog).getByLabelText("模型 ID")).toHaveValue("second");
    expect(mocks.providersSaveConnection).not.toHaveBeenCalled();
    expect(within(dialog).queryByText(/录音|语音额度/)).toBeNull();
  });

  it("保存已成功但加载失败时只重试加载，不重复保存", async () => {
    mocks.providersSaveConnection.mockResolvedValue({ providerId: "custom", modelIds: ["a"] });
    mocks.internalReload.mockRejectedValueOnce(new Error("运行时暂不可用")).mockResolvedValue(undefined);
    render(<ModelConnectionsPanel />);
    fireEvent.click(await screen.findByRole("button", { name: "添加模型连接" }));
    const dialog = screen.getByRole("dialog", { name: "添加模型连接" });
    fireEvent.change(within(dialog).getByLabelText("模型服务商"), { target: { value: "custom" } });
    fireEvent.change(within(dialog).getByLabelText("服务地址（Base URL）"), { target: { value: "https://test.example/v1" } });
    fireEvent.change(within(dialog).getByLabelText("API 密钥"), { target: { value: "test-key" } });
    fireEvent.change(within(dialog).getByLabelText("模型 ID"), { target: { value: "a" } });
    fireEvent.click(within(dialog).getByRole("button", { name: "保存" }));
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
    await openFirstConnection();
    fireEvent.click(screen.getByRole("button", { name: "测试模型 first-model" }));
    mocks.providersList.mockResolvedValue({ providers: [importProvider], models: [{ modelId: "new/model", providerId: importProvider.id }] });
    act(() => useOrgSessionStore.getState().setSession({ loggedIn: true, serverUrl: "https://next-org.test" }));
    await screen.findByText("远端模型 ID：new/model");
    expect(screen.queryByText("远端模型 ID：first-model")).toBeNull();
    await act(async () => finishOldTest());
    expect(screen.queryByText(/测试通过/)).toBeNull();
  });

  it("空目录之后收到组织配置更新时不保留旧的空状态", async () => {
    let changed!: () => void;
    mocks.orgSession.mockResolvedValue({ loggedIn: true, serverUrl: "https://org.test", bootstrap: { scopes: [{ id: "org", kind: "org", name: "组织" }] } });
    mocks.listenOrgModelsChanged.mockImplementationOnce(async callback => { changed = callback; return () => {}; });
    mocks.orgSyncModelConfig.mockResolvedValue({ configured: false, syncedAt: 1 });
    render(<ModelConnectionsPanel />);
    fireEvent.click(await screen.findByRole("button", { name: "管理模型连接" }));
    fireEvent.click(screen.getByRole("button", { name: "刷新组织模型" }));
    await waitFor(() => expect(screen.getByRole("region", { name: "组织模型状态" })).toHaveTextContent("组织暂未提供模型"));
    mocks.providersList.mockResolvedValue({ providers: [{ ...importProvider, source: "organization", managed: true }], models: [{ modelId: "new-org-model", providerId: importProvider.id }] });
    act(() => changed());
    await waitFor(() => expect(screen.getByRole("region", { name: "组织模型状态" })).toHaveTextContent("由组织管理员维护"));
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
    fireEvent.click(await screen.findByRole("button", { name: "添加模型连接" }));
    const dialog = screen.getByRole("dialog", { name: "添加模型连接" });
    fireEvent.change(within(dialog).getByLabelText("模型服务商"), { target: { value: service } });
    fireEvent.change(within(dialog).getByLabelText("API 密钥"), { target: { value: "isolated-test-key" } });
    fireEvent.change(within(dialog).getByLabelText("模型 ID"), { target: { value: "test-model" } });
    fireEvent.click(within(dialog).getByRole("button", { name: "保存" }));
    await screen.findByRole("region", { name: `${label}连接详情` });
    expect(mocks.providersSaveConnection).toHaveBeenCalledExactlyOnceWith(
      expect.objectContaining({ providerKind: "custom", apiBackend: "chat_completions", authScheme: "bearer", baseUrl }),
      [expect.objectContaining({ remoteModelId: "test-model" })], true,
    );
    expect(mocks.providersTestModelConnection).not.toHaveBeenCalled();
    expect(mocks.agentsDefaultsSave).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "编辑连接" }));
    expect(screen.getByLabelText("模型服务商")).toHaveValue(service);
    expect(screen.getByLabelText("API 密钥")).toHaveValue("");
    expect(screen.getByLabelText("已配置模型")).toHaveTextContent("test-model");
  });

  it("先选择服务商，必填错误定位到对应字段，密钥帮助不提交配置", async () => {
    mocks.openExternalUrl.mockResolvedValue(undefined);
    render(<ModelConnectionsPanel />);
    fireEvent.click(await screen.findByRole("button", { name: "添加模型连接" }));
    const dialog = screen.getByRole("dialog", { name: "添加模型连接" });
    fireEvent.click(within(dialog).getByRole("button", { name: "保存" }));
    expect(within(dialog).getByLabelText("模型服务商")).toHaveAttribute("aria-invalid", "true");
    expect(within(dialog).getByLabelText("模型服务商")).toHaveFocus();
    fireEvent.change(within(dialog).getByLabelText("模型服务商"), { target: { value: "deepseek" } });
    fireEvent.click(within(dialog).getByRole("button", { name: "获取密钥" }));
    expect(mocks.openExternalUrl).toHaveBeenCalledExactlyOnceWith("https://platform.deepseek.com");
    fireEvent.click(within(dialog).getByRole("button", { name: "保存" }));
    expect(within(dialog).getByLabelText("API 密钥")).toHaveAttribute("aria-invalid", "true");
    expect(within(dialog).getByLabelText("API 密钥")).toHaveFocus();
    expect(mocks.providersSaveConnection).not.toHaveBeenCalled();
  });

});
