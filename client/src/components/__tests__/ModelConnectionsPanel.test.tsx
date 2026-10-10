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
import { resetOrgSessionMirror } from "@/stores/org-session-store";

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
    vi.clearAllMocks();
    resetOrgSessionMirror();
    mocks.providersList.mockResolvedValue({ providers: [], models: [] });
    mocks.agentsDefaultsGet.mockResolvedValue(defaults);
    mocks.agentAuthStatus.mockResolvedValue({ ready: true, defaultModelId: undefined });
    mocks.internalReload.mockResolvedValue(undefined);
    mocks.orgSession.mockResolvedValue({ loggedIn: false });
    mocks.listenOrgModelsChanged.mockResolvedValue(() => {});
  });

  it("中文输入 Model ID 时 Enter 不添加，关闭草稿先确认", async () => {
    render(<ModelConnectionsPanel />);
    await screen.findByText("还没有可用模型");
    const opener = screen.getAllByRole("button", { name: "添加个人连接" })[0];
    opener.focus();
    fireEvent.click(opener);
    const dialog = screen.getByRole("dialog", { name: "添加个人连接" });
    const input = within(dialog).getByLabelText("模型名称 / ID");
    fireEvent.change(input, { target: { value: "候选模型" } });
    fireEvent.keyDown(input, { key: "Enter", isComposing: true });
    expect(input).toHaveValue("候选模型");
    fireEvent.keyDown(input, { key: "Enter", keyCode: 229 });
    expect(input).toHaveValue("候选模型");
    fireEvent.keyDown(input, { key: "Enter" });
    expect(input).toHaveValue("");
    expect(within(dialog).getByText("候选模型")).toBeInTheDocument();
    fireEvent.keyDown(document, { key: "Escape" });
    expect(screen.getByRole("alertdialog", { name: "舍弃未保存的修改？" })).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "继续编辑" }));
    expect(within(dialog).getByText("候选模型")).toBeInTheDocument();
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
    fireEvent.click(await screen.findByRole("button", { name: "同步模型" }));
    const dialog = screen.getByRole("dialog", { name: "同步模型" });
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
    fireEvent.click(await screen.findByRole("button", { name: "同步模型" }));
    const dialog = screen.getByRole("dialog", { name: "同步模型" });
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
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "同步模型" })).not.toBeInTheDocument());
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

    expect(await screen.findByText("组织托管")).toBeInTheDocument();
    expect(screen.getByText("由组织安全配置 · 不可查看")).toBeInTheDocument();
    expect(screen.getByText("组织提供", { selector: "strong" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "编辑连接" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "删除连接" })).not.toBeInTheDocument();
    expect(screen.getByText("远端模型 ID：MiniMax-M3")).toBeInTheDocument();
    expect(screen.queryByText(/此连接使用 HTTP 明文传输/)).not.toBeInTheDocument();
  });

  it("内置模型无需密钥且显示实际默认模型", async () => {
    mocks.agentAuthStatus.mockResolvedValue({ ready: true, defaultModelId: "echoagent-ojlab/chat-xc" });
    mocks.providersList.mockResolvedValue({
      providers: [{
        id: "echoagent-ojlab",
        providerKind: "custom",
        label: "内置模型",
        source: "builtin",
        managed: true,
        credentialConfigured: true,
        baseUrl: "http://123.56.188.16:8088/v1",
        apiBackend: "chat_completions",
      }],
      models: [
        { modelId: "echoagent-ojlab/chat-xc", remoteModelId: "chat-xc", providerId: "echoagent-ojlab", name: "chat-xc", contextWindow: 262144, maxOutputTokens: 8192, managed: true },
        { modelId: "echoagent-ojlab/chat-glm", remoteModelId: "chat-glm", providerId: "echoagent-ojlab", name: "chat-glm", contextWindow: 131072, maxOutputTokens: 8192, managed: true },
        { modelId: "echoagent-ojlab/chat-qwen", remoteModelId: "chat-qwen", providerId: "echoagent-ojlab", name: "chat-qwen", contextWindow: 262144, maxOutputTokens: 65536, managed: true },
      ],
    });
    render(<ModelConnectionsPanel />);
    expect(await screen.findByText("此服务无需 API Key")).toBeInTheDocument();
    expect(screen.queryByText(/此连接使用 HTTP 明文传输/)).not.toBeInTheDocument();
    expect(screen.getByText("内置只读")).toBeInTheDocument();
    expect(screen.getByText("当前自动使用")).toBeInTheDocument();
    expect(screen.getAllByText("上下文 262,144 · 最大输出 8,192 tokens")).toHaveLength(1);
    expect(screen.getByText("上下文 131,072 · 最大输出 8,192 tokens")).toBeInTheDocument();
    expect(screen.getByText("上下文 262,144 · 最大输出 65,536 tokens")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "编辑连接" })).not.toBeInTheDocument();
  });

  it("标题和无纵向溢出的连接列表可继续滚动详情，移动布局交由原生滚动", async () => {
    mocks.providersList.mockResolvedValue({
      providers: [{ id: "custom", providerKind: "custom", label: "工作", source: "personal", baseUrl: "https://example.com/v1" }],
      models: [{ modelId: "model-a", providerId: "custom" }],
    });
    const { container } = render(<ModelConnectionsPanel />);
    const detail = await screen.findByRole("region", { name: "工作连接详情" });
    const sourceList = screen.getByLabelText("模型连接");
    const panel = container.querySelector<HTMLElement>(".model-connections")!;
    panel.style.overflowY = "hidden";
    Object.defineProperties(detail, {
      scrollHeight: { configurable: true, value: 1000 },
      clientHeight: { configurable: true, value: 200 },
    });
    Object.defineProperties(sourceList, {
      scrollHeight: { configurable: true, value: 200 },
      clientHeight: { configurable: true, value: 200 },
    });

    fireEvent.wheel(screen.getByRole("heading", { name: "模型与连接" }), { deltaY: 120 });
    expect(detail.scrollTop).toBe(120);
    fireEvent.wheel(sourceList, { deltaY: 120 });
    expect(detail.scrollTop).toBe(240);

    Object.defineProperty(sourceList, "scrollHeight", { configurable: true, value: 500 });
    fireEvent.wheel(sourceList, { deltaY: 120 });
    expect(detail.scrollTop).toBe(240);

    panel.style.overflowY = "auto";
    fireEvent.wheel(screen.getByRole("heading", { name: "模型与连接" }), { deltaY: 120 });
    expect(detail.scrollTop).toBe(240);
  });

  it("个人 HTTP 连接仍显示明文传输提醒", async () => {
    mocks.providersList.mockResolvedValue({
      providers: [{ id: "custom", providerKind: "custom", label: "个人连接", source: "personal", baseUrl: "http://example.com/v1" }],
      models: [{ modelId: "model-a", providerId: "custom" }],
    });
    render(<ModelConnectionsPanel />);
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
    expect(await screen.findByText("已设默认 · 当前使用")).toBeInTheDocument();
    expect(screen.queryByText("当前自动使用")).not.toBeInTheDocument();
  });

  it("已设默认模型不在目录中时解释自动回退", async () => {
    mocks.agentsDefaultsGet.mockResolvedValue({ ...defaults, defaultModel: "missing-model" });
    mocks.agentAuthStatus.mockResolvedValue({ ready: true, defaultModelId: "model-a" });
    mocks.providersList.mockResolvedValue({
      providers: [{ id: "custom", providerKind: "custom", source: "personal", baseUrl: "https://example.com/v1" }],
      models: [{ modelId: "model-a", providerId: "custom" }],
    });
    render(<ModelConnectionsPanel />);
    expect(await screen.findByText(/已设默认模型「missing-model」不在当前模型目录中/)).toBeInTheDocument();
    expect(screen.getByText("当前自动使用")).toBeInTheDocument();
  });

  it("手动编辑模型可设置并清除最大输出", async () => {
    mocks.providersList.mockResolvedValue({
      providers: [{ id: "custom", providerKind: "custom", source: "personal", baseUrl: "https://example.com/v1", contextWindow: 16000 }],
      models: [{ modelId: "model-a", remoteModelId: "model-a", providerId: "custom", maxOutputTokens: 8192 }],
    });
    mocks.providersSaveConnection.mockResolvedValue({ providerId: "custom", modelIds: ["model-a"] });
    render(<ModelConnectionsPanel />);
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

    const detail = await screen.findByRole("region", { name: "工作用连接详情" });
    expect(within(detail).getByText("7 个")).toBeInTheDocument();
    expect(within(detail).getAllByRole("listitem")).toHaveLength(7);
    expect(within(detail).getByText("远端模型 ID：model-7")).toBeInTheDocument();
  });

  it("直接填写 Model ID 后测试实际模型并保存个人连接", async () => {
    mocks.providersTestModelConnection.mockResolvedValue(undefined);
    mocks.providersSaveConnection.mockResolvedValue({
      providerId: "custom",
      modelIds: ["MiniMax-M3"],
    });

    render(<ModelConnectionsPanel />);
    await screen.findByText("还没有可用模型");
    fireEvent.click(screen.getAllByRole("button", { name: "添加个人连接" })[0]);

    const dialog = screen.getByRole("dialog", { name: "添加个人连接" });
    fireEvent.change(within(dialog).getByLabelText(/连接名称/), { target: { value: "我的 MiniMax" } });
    fireEvent.change(within(dialog).getByLabelText("Base URL"), { target: { value: "https://api.minimaxi.com/v1" } });
    fireEvent.change(within(dialog).getByLabelText("API Key"), { target: { value: "sk-test" } });
    fireEvent.change(within(dialog).getByLabelText("模型名称 / ID"), { target: { value: "MiniMax-M3" } });
    fireEvent.click(within(dialog).getByRole("button", { name: "测试并保存" }));

    await waitFor(() => {
      expect(mocks.providersTestModelConnection).toHaveBeenCalledWith(
        expect.objectContaining({
          baseUrl: "https://api.minimaxi.com/v1",
          apiKey: "sk-test",
        }),
        "MiniMax-M3",
      );
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

  it("指定服务的个人连接可以留空 API Key", async () => {
    mocks.providersTestModelConnection.mockResolvedValue(undefined);
    mocks.providersSaveConnection.mockResolvedValue({ providerId: "custom", modelIds: ["chat-xc"] });
    render(<ModelConnectionsPanel />);
    await screen.findByText("还没有可用模型");
    fireEvent.click(screen.getAllByRole("button", { name: "添加个人连接" })[0]);
    const dialog = screen.getByRole("dialog", { name: "添加个人连接" });
    fireEvent.change(within(dialog).getByLabelText("Base URL"), { target: { value: "http://123.56.188.16:8088/v1" } });
    fireEvent.change(within(dialog).getByLabelText("模型名称 / ID"), { target: { value: "chat-xc" } });
    expect(within(dialog).getByText("此服务无需 API Key")).toBeInTheDocument();
    expect(within(dialog).getByLabelText("API Key")).toBeDisabled();
    fireEvent.click(within(dialog).getByLabelText(/允许此个人连接使用 HTTP/));
    fireEvent.click(within(dialog).getByRole("button", { name: "测试并保存" }));
    await waitFor(() => expect(mocks.providersSaveConnection).toHaveBeenCalledWith(
      expect.objectContaining({ baseUrl: "http://123.56.188.16:8088/v1", apiKey: undefined, allowInsecureHttp: true }),
      [expect.objectContaining({ remoteModelId: "chat-xc" })],
      true,
    ));
  });

  it("OpenAI 作为 Responses 模型 Provider 接入而不是独立 Agent Runtime", async () => {
    render(<ModelConnectionsPanel />);
    await screen.findByText("还没有可用模型");
    fireEvent.click(screen.getAllByRole("button", { name: "添加个人连接" })[0]);

    const dialog = screen.getByRole("dialog", { name: "添加个人连接" });
    fireEvent.change(within(dialog).getByLabelText("接口类型"), {
      target: { value: "openai" },
    });

    expect(within(dialog).getByLabelText("Base URL")).toHaveValue("https://api.openai.com/v1");
    expect(within(dialog).getByText("使用 OpenAI Platform API Key；ChatGPT 订阅登录不能作为模型 API 凭据。")).toBeInTheDocument();
    fireEvent.click(within(dialog).getByRole("button", { name: "高级设置" }));
    expect(within(dialog).getByDisplayValue("Responses")).toBeInTheDocument();
  });

  it("连接编辑器具有初始焦点、Tab 圈定、Escape 与触发器恢复", async () => {
    render(<ModelConnectionsPanel />);
    await screen.findByText("还没有可用模型");
    const opener = screen.getAllByRole("button", { name: "添加个人连接" })[0];
    opener.focus();
    fireEvent.click(opener);

    const dialog = screen.getByRole("dialog", { name: "添加个人连接" });
    expect(within(dialog).getByLabelText(/连接名称/)).toHaveFocus();
    const last = within(dialog).getByRole("button", { name: "测试并保存" });
    last.focus();
    fireEvent.keyDown(document, { key: "Tab" });
    expect(within(dialog).getByRole("button", { name: "关闭" })).toHaveFocus();

    fireEvent.keyDown(document, { key: "Escape" });
    expect(screen.queryByRole("dialog", { name: "添加个人连接" })).toBeNull();
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
    fireEvent.click(screen.getAllByRole("button", { name: "添加个人连接" })[0]);
    const dialog = screen.getByRole("dialog", { name: "添加个人连接" });
    fireEvent.change(within(dialog).getByLabelText("Base URL"), { target: { value: "https://api.example.com/v1" } });
    fireEvent.change(within(dialog).getByLabelText("API Key"), { target: { value: "sk-test" } });
    fireEvent.change(within(dialog).getByLabelText("模型名称 / ID"), { target: { value: "model-a" } });
    fireEvent.click(within(dialog).getByRole("button", { name: "直接保存" }));

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
    await screen.findByText("现有连接", { selector: "h3" });
    fireEvent.click(screen.getByRole("button", { name: "编辑连接" }));
    const dialog = screen.getByRole("dialog", { name: "编辑连接" });
    expect(within(dialog).getByLabelText("API Key")).toHaveValue("");
    fireEvent.click(within(dialog).getByRole("button", { name: "测试并保存" }));

    await waitFor(() => {
      expect(mocks.providersTestModelConnection).toHaveBeenCalledWith(
        expect.objectContaining({ id: "custom", apiKey: undefined, source: "personal" }),
        "model-a",
      );
    });
  });

  it("编辑连接时以当前勾选结果替换该连接的模型", async () => {
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
    await screen.findByText("现有连接", { selector: "h3" });
    fireEvent.click(screen.getByRole("button", { name: "编辑连接" }));
    const dialog = screen.getByRole("dialog", { name: "编辑连接" });
    fireEvent.click(within(dialog).getByRole("button", { name: "获取模型列表（可选）" }));

    const modelB = await within(dialog).findByRole("checkbox", { name: /model-b/ });
    expect(modelB).toBeChecked();
    fireEvent.click(modelB);
    fireEvent.click(within(dialog).getByRole("button", { name: "直接保存" }));

    await waitFor(() => {
      expect(mocks.providersSaveConnection).toHaveBeenCalledWith(
        expect.objectContaining({ id: "custom", apiKey: undefined }),
        [expect.objectContaining({ modelId: "model-a", remoteModelId: "model-a", maxOutputTokens: 8192 })],
        true,
      );
    });
  });

  it("服务端不支持 models 接口时仍可手填 Model ID 并直接保存", async () => {
    mocks.providersTestConnection.mockRejectedValue(new Error("404 /models"));
    mocks.providersSaveConnection.mockResolvedValue({
      providerId: "custom",
      modelIds: ["private-model-v2"],
    });

    render(<ModelConnectionsPanel />);
    await screen.findByText("还没有可用模型");
    fireEvent.click(screen.getAllByRole("button", { name: "添加个人连接" })[0]);
    const dialog = screen.getByRole("dialog", { name: "添加个人连接" });
    fireEvent.change(within(dialog).getByLabelText("Base URL"), { target: { value: "https://private.example/v1" } });
    fireEvent.change(within(dialog).getByLabelText("API Key"), { target: { value: "private-key" } });
    fireEvent.change(within(dialog).getByLabelText("模型名称 / ID"), { target: { value: "private-model-v2" } });

    fireEvent.click(within(dialog).getByRole("button", { name: "获取模型列表（可选）" }));
    expect(await within(dialog).findByText(/这不影响手动填写、测试和保存/)).toBeInTheDocument();
    fireEvent.click(within(dialog).getByRole("button", { name: "直接保存" }));

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
    await screen.findByText("还没有可用模型");
    fireEvent.click(screen.getAllByRole("button", { name: "添加个人连接" })[0]);
    const dialog = screen.getByRole("dialog", { name: "添加个人连接" });
    const baseUrl = within(dialog).getByLabelText("Base URL");
    fireEvent.change(baseUrl, { target: { value: "http://example.com/:60100/v1" } });
    fireEvent.change(within(dialog).getByLabelText("API Key"), { target: { value: "test-key" } });
    fireEvent.click(within(dialog).getByRole("button", { name: "获取模型列表（可选）" }));
    expect(within(dialog).getByText(/端口号位置不对/)).toBeInTheDocument();
    expect(mocks.providersTestConnection).not.toHaveBeenCalled();

    fireEvent.change(baseUrl, { target: { value: "http://example.com:60100/v1" } });
    fireEvent.click(within(dialog).getByRole("button", { name: "获取模型列表（可选）" }));
    expect(within(dialog).getByText(/使用 HTTP 前，请确认/)).toBeInTheDocument();
    expect(mocks.providersTestConnection).not.toHaveBeenCalled();

    fireEvent.click(within(dialog).getByLabelText(/允许此个人连接使用 HTTP/));
    fireEvent.click(within(dialog).getByRole("button", { name: "获取模型列表（可选）" }));
    await waitFor(() => expect(mocks.providersTestConnection).toHaveBeenCalledWith(
      expect.objectContaining({ baseUrl: "http://example.com:60100/v1", allowInsecureHttp: true }),
    ));

    fireEvent.change(baseUrl, { target: { value: "http://123.56.188.16:8088/v1" } });
    fireEvent.click(within(dialog).getByRole("button", { name: "获取模型列表（可选）" }));
    expect(within(dialog).getByText(/使用 HTTP 前，请确认/)).toBeInTheDocument();
    expect(mocks.providersTestConnection).toHaveBeenCalledTimes(1);
    fireEvent.click(within(dialog).getByLabelText(/允许此个人连接使用 HTTP/));
    fireEvent.click(within(dialog).getByRole("button", { name: "获取模型列表（可选）" }));
    await waitFor(() => expect(mocks.providersTestConnection).toHaveBeenCalledTimes(2));
    expect(mocks.providersTestConnection).toHaveBeenLastCalledWith(
      expect.objectContaining({ baseUrl: "http://123.56.188.16:8088/v1", allowInsecureHttp: true }),
    );

    fireEvent.change(baseUrl, { target: { value: "https://secure.example.com/v1" } });
    fireEvent.change(within(dialog).getByLabelText("API Key"), { target: { value: "test-key" } });
    expect(within(dialog).queryByLabelText(/允许此个人连接使用 HTTP/)).not.toBeInTheDocument();
    fireEvent.click(within(dialog).getByRole("button", { name: "获取模型列表（可选）" }));
    await waitFor(() => expect(mocks.providersTestConnection).toHaveBeenCalledTimes(3));
    expect(mocks.providersTestConnection).toHaveBeenLastCalledWith(
      expect.objectContaining({ baseUrl: "https://secure.example.com/v1", allowInsecureHttp: false }),
    );
  });

  it("实际模型测试失败时不保存，但允许用户确认后直接保存", async () => {
    mocks.providersTestModelConnection.mockRejectedValue(new Error("自定义网关需要额外参数"));
    mocks.providersSaveConnection.mockResolvedValue({
      providerId: "custom",
      modelIds: ["special-model"],
    });

    render(<ModelConnectionsPanel />);
    await screen.findByText("还没有可用模型");
    fireEvent.click(screen.getAllByRole("button", { name: "添加个人连接" })[0]);
    const dialog = screen.getByRole("dialog", { name: "添加个人连接" });
    fireEvent.change(within(dialog).getByLabelText("Base URL"), { target: { value: "https://special.example/v1" } });
    fireEvent.change(within(dialog).getByLabelText("API Key"), { target: { value: "special-key" } });
    fireEvent.change(within(dialog).getByLabelText("模型名称 / ID"), { target: { value: "special-model" } });
    fireEvent.click(within(dialog).getByRole("button", { name: "测试并保存" }));

    expect(await within(dialog).findByText(/测试失败，尚未保存/)).toBeInTheDocument();
    expect(mocks.providersSaveConnection).not.toHaveBeenCalled();

    fireEvent.click(within(dialog).getByRole("button", { name: "直接保存" }));
    await waitFor(() => expect(mocks.providersSaveConnection).toHaveBeenCalledTimes(1));
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
    await screen.findByText("个人连接", { selector: "h3" });
    fireEvent.click(screen.getByRole("button", { name: "删除连接" }));
    const dialog = screen.getByRole("alertdialog", { name: "删除连接“个人连接”？" });
    fireEvent.click(within(dialog).getByRole("button", { name: "删除连接" }));

    await waitFor(() => expect(mocks.providersDeleteProvider).toHaveBeenCalledTimes(1));
    await waitFor(() => expect(screen.queryByRole("alertdialog")).toBeNull());
    expect(await screen.findByText(/shell refresh failed/)).toHaveTextContent("配置已保存");
    expect(mocks.providersDeleteProvider).toHaveBeenCalledTimes(1);
  });
});
