import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { ExpertsTab } from "../experts-panel/experts/ExpertsTab";
import { AssistantsPanel } from "../AssistantsPanel";
import { SkillsTab } from "../experts-panel/skills/SkillsTab";
import { ConnectorsTab } from "../experts-panel/connectors/ConnectorsTab";
import { PluginsPanel } from "../PluginsPanel";
import { useSessionStore } from "@/stores/session-store";

const api = vi.hoisted(() => ({
  agentsList: vi.fn(), skillsList: vi.fn(), internalReload: vi.fn(), pluginsList: vi.fn(), pluginsAction: vi.fn(),
  mcpList: vi.fn(), mcpToggle: vi.fn(), mcpConfigPath: vi.fn(), mcpConfigRead: vi.fn(), mcpConfigSave: vi.fn(),
  onMcpStatusEvent: vi.fn(),
  listenOrgSkillsChanged: vi.fn(),
  agentsSave: vi.fn(), agentsDelete: vi.fn(),
}));
vi.mock("@/lib/agent-client", async (original) => ({ ...await original<object>(), ...api }));
vi.mock("@/lib/org-client", async (original) => ({
  ...await original<object>(), listenOrgSkillsChanged: api.listenOrgSkillsChanged,
}));

const expert = { name: "测试专家", path: "/agents/review.md", scope: "user", raw: "检查需求", description: "评审需求" };
const skill = { name: "review", displayName: "测试技能", scope: "user", enabled: true, description: "评审需求" };
const plugin = { id: "user/review", name: "测试插件", enabled: true, trusted: true, root: "/plugins/review" };
const connector = {
  name: "review", displayName: "测试连接器", transport: "stdio", target: "review-tool", enabled: true,
  source: "user", status: "ready", live: false, editable: true, tools: [], args: [], env: {}, setupValues: {},
};

describe("统一扩展管理的真实页面交互", () => {
  beforeEach(() => {
    vi.resetAllMocks();
    localStorage.clear();
    useSessionStore.getState().setSession(null);
    api.agentsList.mockResolvedValue([expert]);
    api.skillsList.mockResolvedValue([skill]);
    api.internalReload.mockResolvedValue(undefined);
    api.pluginsList.mockResolvedValue({ plugins: [plugin] });
    api.pluginsAction.mockResolvedValue({ status: "success" });
    api.mcpList.mockResolvedValue([connector]);
    api.onMcpStatusEvent.mockResolvedValue(() => {});
    api.listenOrgSkillsChanged.mockResolvedValue(() => {});
    api.mcpConfigPath.mockResolvedValue("/review/mcp.json");
    api.mcpConfigRead.mockResolvedValue({ filePath: "/review/mcp.json", content: '{"mcpServers":{"existing":{}}}' });
    api.mcpConfigSave.mockResolvedValue({ appliedLive: false, serverCount: 1, warnings: [] });
    api.mcpToggle.mockResolvedValue({ warnings: [] });
  });

  it.each([
    { title: "我的专家", search: "搜索专家", item: "查看专家 测试专家 详情", page: () => <ExpertsTab pills={null} /> },
    { title: "已安装技能", search: "搜索已安装技能", item: "测试技能", page: () => <SkillsTab pills={null} /> },
    { title: "已配置连接器", search: "搜索连接器", item: "测试连接器", page: () => <ConnectorsTab pills={null} /> },
    { title: "已安装插件", search: "搜索插件", item: "测试插件", page: () => <PluginsPanel sessionId="review-session" /> },
  ])("$title 搜索无结果后可清空并恢复条目", async ({ title, search, item, page }) => {
    render(page());
    expect(screen.getByRole("heading", { name: title })).toBeInTheDocument();
    await waitFor(() => expect(document.body.textContent).toContain(item.startsWith("查看专家") ? "测试专家" : item));
    fireEvent.change(screen.getByRole("textbox", { name: search }), { target: { value: "   测试   " } });
    expect(screen.queryByText(/没有找到与/)).toBeNull();
    if (item.startsWith("查看专家")) expect(screen.getByRole("button", { name: item })).toBeInTheDocument();
    else expect(screen.getByText(item)).toBeInTheDocument();
    fireEvent.change(screen.getByRole("textbox", { name: search }), { target: { value: "不存在的能力" } });
    expect(await screen.findByText(/没有找到与「不存在的能力」匹配/)).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "清空搜索" }));
    expect(screen.getByRole("textbox", { name: search })).toHaveValue("");
    if (item.startsWith("查看专家")) expect(screen.getByRole("button", { name: item })).toBeInTheDocument();
    else expect(screen.getByText(item)).toBeInTheDocument();
  });

  it("专家工具栏刷新能发现新增专家", async () => {
    api.agentsList.mockResolvedValueOnce([]).mockResolvedValueOnce([expert]);
    render(<ExpertsTab pills={null} />);
    await screen.findByText("创建你的第一个专家，或选择下方模板开始");
    fireEvent.click(screen.getByRole("button", { name: "刷新专家" }));
    expect(await screen.findByRole("button", { name: "查看专家 测试专家 详情" })).toBeInTheDocument();
  });

  it("专家模板只预填创建草稿，保留指令和模型标签", async () => {
    render(<ExpertsTab pills={null} />);
    fireEvent.click(await screen.findByRole("button", { name: "代码专家" }));
    const editor = await screen.findByRole("dialog", { name: "创建专家" });
    expect(within(editor).getByRole("textbox", { name: "专家名称" })).toHaveValue("代码专家");
    expect((within(editor).getByRole("textbox", { name: "专家指令" }) as HTMLTextAreaElement).value).toContain("你是一名代码专家");
    expect(within(editor).getByRole("button", { name: "默认" })).toHaveAttribute("aria-pressed", "true");
    expect(within(editor).getByRole("button", { name: "推理" })).toHaveAttribute("aria-pressed", "true");
    expect(api.agentsSave).not.toHaveBeenCalled();
  });

  it("专家卡片可对话，通过更多菜单编辑，删除仍需确认", async () => {
    const onUse = vi.fn();
    render(<AssistantsPanel onUseAssistant={onUse} />);
    fireEvent.click(await screen.findByRole("button", { name: "与测试专家对话" }));
    expect(onUse).toHaveBeenCalledWith(expert);
    expect(screen.queryByText("空闲")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "专家 测试专家更多操作" }));
    fireEvent.click(screen.getByRole("menuitem", { name: "编辑专家 测试专家" }));
    const editor = await screen.findByRole("dialog", { name: "编辑 测试专家" });
    expect(within(editor).getByRole("textbox", { name: "专家名称" })).toHaveValue(expert.name);
    fireEvent.click(within(editor).getByRole("button", { name: "取消" }));
    fireEvent.click(screen.getByRole("button", { name: "专家 测试专家更多操作" }));
    fireEvent.click(screen.getByRole("menuitem", { name: "删除专家 测试专家" }));
    const confirmation = await screen.findByRole("alertdialog");
    expect(api.agentsDelete).not.toHaveBeenCalled();
    fireEvent.click(within(confirmation).getByRole("button", { name: "取消" }));
    expect(screen.getByRole("button", { name: "查看专家 测试专家 详情" })).toBeInTheDocument();
    expect(api.agentsDelete).not.toHaveBeenCalled();
  });

  it("技能刷新失败保留条目，重试成功后恢复正常", async () => {
    api.skillsList.mockResolvedValueOnce([skill]).mockRejectedValueOnce(new Error("目录暂不可用")).mockResolvedValueOnce([skill]);
    render(<SkillsTab pills={null} />);
    await screen.findByText("测试技能");
    const refresh = screen.getByRole("button", { name: "刷新" });
    await waitFor(() => expect(refresh).toBeEnabled());
    fireEvent.click(refresh);
    expect(await screen.findByRole("alert")).toHaveTextContent("目录暂不可用");
    expect(screen.getByRole("alert")).toHaveTextContent("下方保留上次加载的结果");
    expect(screen.getByText("测试技能")).toBeInTheDocument();
    const retry = screen.getByRole("button", { name: "重试" });
    await waitFor(() => expect(retry).toBeEnabled());
    fireEvent.click(retry);
    await waitFor(() => expect(screen.queryByRole("alert")).toBeNull());
    expect(api.internalReload).toHaveBeenCalledTimes(2);
    expect(api.internalReload).toHaveBeenLastCalledWith("skills");
    expect(api.skillsList).toHaveBeenCalledTimes(3);
  });

  it("连接器页面支持无会话启停，继续调用原持久化接口", async () => {
    render(<ConnectorsTab pills={null} />);
    fireEvent.click(await screen.findByRole("checkbox", { name: "停用 MCP 服务 测试连接器" }));
    await waitFor(() => expect(api.mcpToggle).toHaveBeenCalledWith(undefined, "review", false));
    expect(useSessionStore.getState().sessionId).toBeNull();
  });

  it("停用已落盘但会话未确认时保留告警，可原样重试停用", async () => {
    api.mcpToggle.mockResolvedValueOnce({ persisted: true, appliedLive: false,
      warnings: ["新工具调用已阻止，仍有会话未确认卸载"] });
    api.mcpList.mockResolvedValueOnce([connector]).mockResolvedValue([{ ...connector, enabled: false }]);
    render(<ConnectorsTab pills={null} />);
    fireEvent.click(await screen.findByRole("checkbox", { name: "停用 MCP 服务 测试连接器" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("仍有会话未确认卸载");
    const retry = screen.getByRole("button", { name: "重试切换" });
    await waitFor(() => expect(retry).toBeEnabled());
    expect(screen.getByRole("checkbox", { name: "启用 MCP 服务 测试连接器" })).not.toBeChecked();
    fireEvent.click(retry);
    await waitFor(() => expect(screen.queryByRole("alert")).toBeNull());
    expect(api.mcpToggle).toHaveBeenCalledTimes(2);
    expect(api.mcpToggle).toHaveBeenLastCalledWith(undefined, "review", false);
  });

  it("启用已落盘但会话未确认时保留告警，可原样重试启用", async () => {
    api.mcpToggle.mockResolvedValueOnce({ persisted: true, appliedLive: false,
      warnings: ["启用状态已保存，仍有会话未确认恢复"] });
    api.mcpList.mockResolvedValueOnce([{ ...connector, enabled: false }]).mockResolvedValue([connector]);
    render(<ConnectorsTab pills={null} />);
    fireEvent.click(await screen.findByRole("checkbox", { name: "启用 MCP 服务 测试连接器" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("仍有会话未确认恢复");
    const retry = screen.getByRole("button", { name: "重试切换" });
    await waitFor(() => expect(retry).toBeEnabled());
    expect(screen.getByRole("checkbox", { name: "停用 MCP 服务 测试连接器" })).toBeChecked();
    fireEvent.click(retry);
    await waitFor(() => expect(screen.queryByRole("alert")).toBeNull());
    expect(api.mcpToggle).toHaveBeenCalledTimes(2);
    expect(api.mcpToggle).toHaveBeenLastCalledWith(undefined, "review", true);
  });

  it("待启动的连接器不会被显示为零工具故障，地址收进详情后仍可搜索", async () => {
    render(<ConnectorsTab pills={null} />);
    fireEvent.change(screen.getByRole("textbox", { name: "搜索连接器" }), { target: { value: "review-tool" } });
    const name = await screen.findByText("测试连接器");
    expect(screen.getByText("待任务启动")).toBeInTheDocument();
    expect(screen.queryByText("0 个工具")).toBeNull();
    expect(screen.queryByText("review-tool")).toBeNull();
    fireEvent.click(name);
    expect(screen.getByText("review-tool")).toBeInTheDocument();
    expect(screen.getByText("启动任务后将连接服务并获取工具")).toBeInTheDocument();
  });

  it("连接器默认紧凑列表，切换卡片保留已展开详情、控件及视图偏好", async () => {
    const view = render(<ConnectorsTab pills={null} />);
    const name = await screen.findByText("测试连接器");
    const list = screen.getByRole("list", { name: "已配置连接器" });
    expect(list).toHaveAttribute("data-view", "list");
    expect(list).toHaveAttribute("data-layout", "grid");
    expect(screen.queryByText("已启用，启动任务后连接")).toBeNull();
    fireEvent.click(name);
    const main = name.closest("button")!;
    const details = document.getElementById(main.getAttribute("aria-controls")!);
    expect(details).toHaveTextContent("review-tool");
    const toggle = screen.getByRole("checkbox", { name: "停用 MCP 服务 测试连接器" });

    const cards = screen.getByRole("button", { name: "卡片视图" });
    cards.focus();
    fireEvent.click(cards);
    expect(cards).toHaveFocus();
    expect(list).toHaveAttribute("data-view", "cards");
    expect(main).toHaveAttribute("aria-expanded", "true");
    expect(document.getElementById(main.getAttribute("aria-controls")!)).toBe(details);
    expect(screen.getByRole("checkbox", { name: "停用 MCP 服务 测试连接器" })).toBe(toggle);
    expect(api.mcpToggle).not.toHaveBeenCalled();

    view.unmount();
    render(<ConnectorsTab pills={null} />);
    await screen.findByText("测试连接器");
    expect(screen.getByRole("list", { name: "已配置连接器" })).toHaveAttribute("data-view", "cards");
  });

  it("配置入口保留已有内容，拒绝无效 JSON，保存后可回到连接器列表", async () => {
    render(<ConnectorsTab pills={null} />);
    fireEvent.click(await screen.findByRole("button", { name: "配置连接器" }));
    const input = await screen.findByRole("textbox", { name: "MCP 配置内容" });
    expect(input).toHaveValue('{"mcpServers":{"existing":{}}}');
    fireEvent.change(input, { target: { value: "{" } });
    fireEvent.click(screen.getByRole("button", { name: "保存" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("无效的 JSON");
    expect(api.mcpConfigSave).not.toHaveBeenCalled();
    const content = '{"mcpServers":{"configured":{"command":"review"}}}';
    fireEvent.change(input, { target: { value: content } });
    fireEvent.click(screen.getByRole("button", { name: "保存" }));
    await waitFor(() => expect(api.mcpConfigSave).toHaveBeenCalledWith(content, undefined));
    await waitFor(() => expect(screen.getByRole("button", { name: "保存" })).toBeDisabled());
    fireEvent.click(screen.getByRole("button", { name: "返回连接器列表" }));
    expect(await screen.findByRole("heading", { name: "已配置连接器" })).toBeInTheDocument();
  });

  it("读取配置失败时无法用空配置覆盖原文件，重试后才允许编辑", async () => {
    api.mcpConfigRead.mockRejectedValueOnce(new Error("原配置暂不可读"));
    render(<ConnectorsTab pills={null} />);
    fireEvent.click(await screen.findByRole("button", { name: "配置连接器" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("原配置暂不可读");
    expect(screen.queryByRole("textbox", { name: "MCP 配置内容" })).toBeNull();
    expect(screen.getByRole("button", { name: "保存" })).toBeDisabled();
    expect(api.mcpConfigSave).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "重试" }));
    expect(await screen.findByRole("textbox", { name: "MCP 配置内容" })).toHaveValue('{"mcpServers":{"existing":{}}}');
  });

  it("保存失败保留配置草稿，取消时确认后可继续编辑", async () => {
    api.mcpConfigSave.mockRejectedValueOnce(new Error("文件不可写"));
    render(<ConnectorsTab pills={null} />);
    fireEvent.click(await screen.findByRole("button", { name: "配置连接器" }));
    const input = await screen.findByRole("textbox", { name: "MCP 配置内容" });
    const draft = '{"mcpServers":{"draft":{}}}';
    fireEvent.change(input, { target: { value: draft } });
    fireEvent.click(screen.getByRole("button", { name: "保存" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("文件不可写");
    expect(input).toHaveValue(draft);
    fireEvent.click(screen.getByRole("button", { name: "取消" }));
    const confirmation = await screen.findByRole("alertdialog", { name: "放弃未保存的修改？" });
    fireEvent.click(within(confirmation).getByRole("button", { name: "取消" }));
    expect(input).toHaveValue(draft);
  });
});
