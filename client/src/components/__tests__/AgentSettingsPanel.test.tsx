// @vitest-environment jsdom
import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import "@testing-library/jest-dom/vitest";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { AgentSettingsPanel } from "../SettingsSections";

const actions = vi.hoisted(() => ({ get: vi.fn(), save: vi.fn(), reload: vi.fn() }));
vi.mock("@/lib/agent-client", async original => ({
  ...await original<typeof import("@/lib/agent-client")>(),
  subagentsConfigGet: actions.get, subagentsConfigSave: actions.save, internalReload: actions.reload,
}));
const endpointKey = "echoagent.otlp.endpoint";
const endpoint = "https://monitor.example.test/v1/logs";
const developer = () => screen.getByText("开发者选项").closest("details")!;
const monitor = () => screen.getByRole("group", { name: "监控服务配置" });
const field = () => screen.getByRole("textbox", { name: "监控服务地址（OTLP）" });

beforeEach(() => {
  vi.restoreAllMocks(); vi.resetAllMocks(); localStorage.clear();
  actions.get.mockResolvedValue({ maxDepth: 4 });
  actions.save.mockImplementation(async depth => depth);
  actions.reload.mockResolvedValue(undefined);
});

describe("高级设置", () => {
  it("正常打开只有两个折叠入口，不重载或写入任何配置", async () => {
    localStorage.setItem(endpointKey, endpoint);
    const write = vi.spyOn(Storage.prototype, "setItem");
    const remove = vi.spyOn(Storage.prototype, "removeItem");
    const { container } = render(<AgentSettingsPanel />);
    await waitFor(() => expect(actions.get).toHaveBeenCalledOnce());
    const depth = container.querySelector<HTMLInputElement>('input[type="number"]')!;
    await waitFor(() => expect(depth).toHaveValue(4));
    expect(container.querySelectorAll("details")).toHaveLength(2);
    expect(developer()).not.toHaveAttribute("open");
    expect(screen.getByText("配置未生效？").closest("details")).not.toHaveAttribute("open");
    expect(screen.getByText(/监控服务已启用/)).toBeVisible();
    expect(screen.queryByRole("button", { name: "刷新状态" })).toBeNull();
    fireEvent.click(screen.getByText("开发者选项"));
    expect(field()).toHaveValue(endpoint);
    expect(screen.getByRole("spinbutton", { name: "子代理最大层级" })).toHaveValue(4);
    expect(write).not.toHaveBeenCalled(); expect(remove).not.toHaveBeenCalled();
    expect(actions.save).not.toHaveBeenCalled(); expect(actions.reload).not.toHaveBeenCalled();
  });

  it("折叠和撤销保留已保存层级，只有主动保存才调用原有接口", async () => {
    render(<AgentSettingsPanel />); fireEvent.click(screen.getByText("开发者选项"));
    const depth = screen.getByRole("spinbutton", { name: "子代理最大层级" });
    await waitFor(() => expect(depth).toHaveValue(4));
    fireEvent.change(depth, { target: { value: "6" } });
    fireEvent.click(screen.getByText("开发者选项"));
    expect(developer()).toHaveTextContent("有未保存的修改");
    fireEvent.click(screen.getByText("开发者选项"));
    expect(depth).toHaveValue(6);
    fireEvent.click(screen.getByRole("button", { name: "撤销修改" }));
    expect(depth).toHaveValue(4); expect(actions.save).not.toHaveBeenCalled();
    fireEvent.change(depth, { target: { value: "5" } });
    fireEvent.click(within(depth.closest(".agent-runtime-row")!).getByRole("button", { name: "保存" }));
    expect(await screen.findByRole("status")).toHaveTextContent("重启 Agent 后生效");
    expect(actions.save).toHaveBeenCalledExactlyOnceWith(5);
  });

  it("读取失败时展开排查入口，禁止默认值覆盖，重试读取原配置", async () => {
    actions.get.mockRejectedValueOnce(new Error("配置暂不可读")).mockResolvedValue({ maxDepth: 7 });
    render(<AgentSettingsPanel />);
    expect(await screen.findByRole("alert")).toHaveTextContent("配置暂不可读");
    expect(developer()).toHaveAttribute("open");
    const depth = screen.getByRole("spinbutton", { name: "子代理最大层级" });
    expect(depth).toHaveValue(null); expect(depth).toBeDisabled();
    expect(actions.save).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "重试" }));
    await waitFor(() => expect(depth).toHaveValue(7));
    expect(depth).toBeEnabled(); expect(actions.save).not.toHaveBeenCalled();
  });

  it("重载分项调用原接口，pending 防重，失败重试不干扰其他结果", async () => {
    let finish!: () => void;
    actions.reload.mockImplementationOnce(() => new Promise<void>(resolve => { finish = resolve; }))
      .mockRejectedValueOnce(new Error("技能目录暂不可读")).mockResolvedValue(undefined);
    render(<AgentSettingsPanel />); fireEvent.click(screen.getByText("配置未生效？"));
    const mcp = screen.getByRole("button", { name: "重新加载MCP 连接器" });
    const skills = screen.getByRole("button", { name: "重新加载技能目录" });
    fireEvent.click(mcp); fireEvent.click(mcp); fireEvent.click(skills);
    expect(actions.reload).toHaveBeenCalledExactlyOnceWith("mcp_all");
    expect(mcp).toHaveAttribute("aria-busy", "true"); expect(skills).toBeDisabled();
    await act(async () => finish());
    expect(within(screen.getByRole("region", { name: "MCP 连接器" })).getByRole("status")).toHaveTextContent("已重新加载");
    fireEvent.click(skills);
    expect(await screen.findByRole("alert")).toHaveTextContent("技能目录暂不可读");
    fireEvent.click(skills);
    await waitFor(() => expect(screen.queryByRole("alert")).toBeNull());
    fireEvent.click(screen.getByRole("button", { name: "重新加载模型配置" }));
    await waitFor(() => expect(actions.reload.mock.calls).toEqual([["mcp_all"], ["skills"], ["skills"], ["models"]]));
    expect(actions.save).not.toHaveBeenCalled();
  });

  it("监控地址沿用现有校验和存储键，主动保存或关闭才更新", async () => {
    localStorage.setItem(endpointKey, endpoint);
    render(<AgentSettingsPanel />); fireEvent.click(screen.getByText("开发者选项"));
    fireEvent.change(field(), { target: { value: "javascript:invalid" } });
    fireEvent.click(within(monitor()).getByRole("button", { name: "保存" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("无法保存监控配置");
    expect(localStorage.getItem(endpointKey)).toBe(endpoint);
    fireEvent.change(field(), { target: { value: "http://127.0.0.1:4318/v1/logs" } });
    fireEvent.click(within(monitor()).getByRole("button", { name: "保存" }));
    expect(localStorage.getItem(endpointKey)).toBe("http://127.0.0.1:4318/v1/logs");
    expect(within(monitor()).getByRole("status")).toHaveTextContent("重启应用后生效");
    fireEvent.click(screen.getByRole("button", { name: "关闭监控" }));
    expect(localStorage.getItem(endpointKey)).toBeNull(); expect(field()).toHaveValue("");
    expect(within(monitor()).getByRole("status")).toHaveTextContent("监控已关闭");
    expect(actions.save).not.toHaveBeenCalled(); expect(actions.reload).not.toHaveBeenCalled();
  });

  it("保存或关闭监控失败保留原配置和草稿，可就地重试", async () => {
    localStorage.setItem(endpointKey, endpoint);
    render(<AgentSettingsPanel />); fireEvent.click(screen.getByText("开发者选项"));
    const next = "https://new-monitor.example.test/v1/logs";
    fireEvent.change(field(), { target: { value: next } });
    vi.spyOn(Storage.prototype, "setItem").mockImplementationOnce(() => { throw new Error("存储不可写"); });
    fireEvent.click(within(monitor()).getByRole("button", { name: "保存" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("存储不可写");
    expect(field()).toHaveValue(next); expect(localStorage.getItem(endpointKey)).toBe(endpoint);
    fireEvent.click(within(monitor()).getByRole("button", { name: "撤销修改" }));
    expect(field()).toHaveValue(endpoint);
    vi.spyOn(Storage.prototype, "removeItem").mockImplementationOnce(() => { throw new Error("关闭失败"); });
    fireEvent.click(screen.getByRole("button", { name: "关闭监控" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("关闭失败");
    expect(field()).toHaveValue(endpoint); expect(localStorage.getItem(endpointKey)).toBe(endpoint);
    fireEvent.click(screen.getByRole("button", { name: "关闭监控" }));
    expect(localStorage.getItem(endpointKey)).toBeNull();
  });
});
